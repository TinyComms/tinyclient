//! Discord's messages into the one message model: markdown to text and spans, attachments,
//! embeds, stickers, reactions, replies, system lines.

use super::model::{self, seconds, Attachment as Attached, Component, Embed, EmbedImage, Posted, UnfurledMedia};
use crate::adapters::Context;
use base64::Engine;
use protocol::{
    AccountKey, Attachment, Card, ControlKind, ControlOption, ControlStyle, Forwarded, InviteState, Key, MediaFile, MediaKind,
    MentionKind, Message, MessageControl, MessageInvite, MessageKind, Preview, Reaction, ReplyTo, ScheduledEvent, Sender, Span, Style,
    ThreadSummary, Via, MESSAGE_CONTROLS_LIMIT,
};

/// Names for what a message only gives ids for.
pub trait Names {
    fn user(&self, id: &str) -> Option<String>;
    fn role(&self, id: &str) -> Option<String>;
    fn channel(&self, id: &str) -> Option<String>;
    /// A server's name.
    fn space(&self, id: &str) -> Option<String>;
    /// People, roles, or channels an entity select (component types 5–8) can offer, as `(id, name)`.
    fn choices(&self, _kind: u8) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// The files something refers to: each with the address it is fetched from, and the other
/// sizes Discord serves of those that are pictures.
#[derive(Default)]
pub struct Files {
    pub files: Vec<(MediaFile, String)>,
    /// A file's id, and `(longest side, address)` for each larger size.
    pub sizes: Vec<(String, Vec<(u32, String)>)>,
}

impl Files {
    pub fn push(&mut self, file: (MediaFile, String)) {
        self.files.push(file);
    }

    /// Adds what `more` holds that is not here yet.
    pub fn merge(&mut self, more: Files) {
        for file in more.files {
            if !self.files.iter().any(|(known, _)| known.id == file.0.id) {
                self.files.push(file);
            }
        }
        for sizes in more.sizes {
            if !self.sizes.iter().any(|(known, _)| *known == sizes.0) {
                self.sizes.push(sizes);
            }
        }
    }

    /// Registers the files, then their sizes. Before recording whatever refers to them.
    pub fn register(&self, ctx: &Context) -> rusqlite::Result<()> {
        ctx.media(&self.files)?;
        for (id, sizes) in &self.sizes {
            ctx.media_variants(id, sizes)?;
        }
        Ok(())
    }
}

/// Text being built, with the ranges styled so far and the files they refer to.
#[derive(Default)]
pub struct Rich {
    pub text: String,
    pub spans: Vec<Span>,
    pub files: Files,
}

impl Rich {
    fn span(&mut self, start: usize, style: Style) {
        if self.text.len() > start {
            self.spans.push(Span {
                start: start as u32,
                end: self.text.len() as u32,
                style,
            });
        }
    }
}

/// Where the unescaped `delimiter` next closes, searching `text` from `from`.
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

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `<t:1618953630:f>` as words, in the machine's own time zone.
fn timestamp(unix: i64, style: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    let unix = unix as libc::time_t;
    // SAFETY: platform localtime helpers fill the struct we hand them.
    unsafe {
        #[cfg(unix)]
        libc::localtime_r(&unix, &mut local);
        #[cfg(windows)]
        {
            if libc::localtime_s(&mut local, &unix) != 0 {
                return String::new();
            }
        }
    }
    let local = local;
    let month = MONTHS[(local.tm_mon.clamp(0, 11)) as usize];
    let weekday = DAYS[(local.tm_wday.clamp(0, 6)) as usize];
    let (day, year) = (local.tm_mday, local.tm_year + 1900);
    let time = format!("{}:{:02}", local.tm_hour, local.tm_min);
    match style {
        "t" => time,
        "T" => format!("{time}:{:02}", local.tm_sec),
        "d" => format!("{year}-{:02}-{day:02}", local.tm_mon + 1),
        "D" => format!("{day} {month} {year}"),
        "F" => format!("{weekday}, {day} {month} {year} {time}"),
        // Relative times ("in 2 hours") would go stale in a stored message, so they are dated.
        _ => format!("{day} {month} {year} {time}"),
    }
}

/// Formats a Discord channel or message link (`discord.com/channels/{guild|@me}/{channel}[/{msg}]`,
/// including `ptb.`, `canary.`, and `discordapp.com`) as a channel/message chip carrying both
/// `Style::Mention { kind: MentionKind::Channel }` and `Style::Link { url }` so clicking resolves
/// in-app and falls back to the browser if the server/channel is unknown.
fn discord_channel_link(
    rich: &mut Rich,
    url: &str,
    masked_label: Option<&str>,
    names: &dyn Names,
    guild: Option<&str>,
) -> bool {
    if !protocol::link::is_web(url) {
        return false;
    }
    let Some(parsed) = protocol::link::parse_network_link(url) else {
        return false;
    };
    if parsed.network != protocol::Network::Discord {
        return false;
    }
    let start = rich.text.len();
    match masked_label {
        Some(label) => inline(rich, label, names, guild),
        None => {
            let channel_shown = match names.channel(&parsed.channel) {
                Some(name) => format!("#{name}"),
                None => "# No Access".to_owned(),
            };
            if parsed.message_id.is_some() {
                rich.text.push_str(&format!("{channel_shown} › message"));
            } else {
                rich.text.push_str(&channel_shown);
            }
        }
    }
    rich.span(start, Style::Mention { kind: MentionKind::Channel, id: parsed.channel });
    rich.span(start, Style::Link { url: url.to_owned() });
    true
}

/// One `<...>` token: a mention, a custom emoji, a time, a server navigation token, or a link
/// without a preview. Returns how many bytes of `text` it took.
fn angle(rich: &mut Rich, text: &str, names: &dyn Names, guild: Option<&str>) -> Option<usize> {
    let end = text.find('>')?;
    let inner = &text[1..end];
    let start = rich.text.len();
    let mention = |rich: &mut Rich, shown: String, kind: MentionKind, id: &str| {
        rich.text.push_str(&shown);
        rich.span(start, Style::Mention { kind, id: id.to_owned() });
    };
    if let Some(id) = inner.strip_prefix("@&").filter(|id| digits(id)) {
        let name = names.role(id).unwrap_or_else(|| "role".to_owned());
        mention(rich, format!("@{name}"), MentionKind::Role, id);
    } else if let Some(id) = inner.strip_prefix('@').map(|id| id.trim_start_matches('!')).filter(|id| digits(id)) {
        let name = names.user(id).unwrap_or_else(|| "someone".to_owned());
        mention(rich, format!("@{name}"), MentionKind::User, id);
    } else if let Some(id) = inner.strip_prefix('#').filter(|id| digits(id)) {
        let shown = match names.channel(id) {
            Some(name) => format!("#{name}"),
            None => "# No Access".to_owned(),
        };
        mention(rich, shown, MentionKind::Channel, id);
    } else if let Some(special) = inner.strip_prefix("id:") {
        let (label, suffix) = match special {
            "browse" => ("Browse Channels", "channel-browser"),
            "customize" => ("Channels & Roles", "customize-community"),
            "guide" | "home" => ("Server Guide", "home"),
            _ => return None,
        };
        rich.text.push_str(label);
        if let Some(guild) = guild.filter(|g| digits(g)) {
            rich.span(start, Style::Link { url: format!("https://discord.com/channels/{guild}/{suffix}") });
        }
    } else if let Some((name, id, animated)) = protocol::discord_emoji_token(&text[..=end]) {
        let (file, url) = model::emoji(id, animated);
        rich.text.push_str(&format!(":{name}:"));
        rich.span(start, Style::Emoji { file: file.clone() });
        rich.files.push((file, url));
    } else if let Some(time) = inner.strip_prefix("t:") {
        let (unix_str, style_str) = time.split_once(':').unwrap_or((time, "f"));
        let unix: i64 = unix_str.parse().ok()?;
        let form = match style_str {
            "t" => 't',
            "T" => 'T',
            "d" => 'd',
            "D" => 'D',
            "f" => 'f',
            "F" => 'F',
            "R" => 'R',
            _ => 'f',
        };
        let fallback = timestamp(unix, style_str);
        if fallback.is_empty() {
            return None;
        }
        rich.text.push_str(&fallback);
        rich.span(start, Style::Time { unix, form });
    } else if let Some(command) = inner.strip_prefix('/') {
        let (name, id) = command.rsplit_once(':')?;
        if !digits(id) || name.is_empty() {
            return None;
        }
        mention(rich, format!("/{name}"), MentionKind::Command, id);
    } else if inner.starts_with("https://") || inner.starts_with("http://") {
        if !discord_channel_link(rich, inner, None, names, guild) {
            rich.text.push_str(inner);
            rich.span(start, Style::Link { url: inner.to_owned() });
        }
    } else {
        return None;
    }
    Some(end + 1)
}

/// The length of the bare link `text` starts with.
fn bare_link(text: &str) -> usize {
    let mut end = text.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '|')).unwrap_or(text.len());
    // Punctuation after a link belongs to the sentence, and so does an unmatched bracket.
    while let Some(last) = text[..end].chars().next_back() {
        let unmatched = last == ')' && !text[..end].contains('(');
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~') || unmatched {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Inline markdown of one line.
fn inline(rich: &mut Rich, text: &str, names: &dyn Names, guild: Option<&str>) {
    const PAIRS: [(&str, Option<Style>); 5] = [
        ("**", Some(Style::Bold)),
        ("__", Some(Style::Underline)),
        ("~~", Some(Style::Strike)),
        ("||", Some(Style::Spoiler)),
        ("``", None),
    ];
    let mut at = 0;
    'scan: while at < text.len() {
        let rest = &text[at..];
        let start = rich.text.len();
        let first = rest.chars().next().unwrap_or(' ');
        // What comes before decides whether `_` and a link start here.
        let after_word = text[..at].chars().next_back().is_some_and(char::is_alphanumeric);

        if first == '\\' {
            if let Some(escaped) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                rich.text.push(escaped);
                at += 1 + escaped.len_utf8();
                continue;
            }
        }
        for (delimiter, style) in &PAIRS {
            if !rest.starts_with(delimiter) {
                continue;
            }
            if let Some(close) = closing(rest, delimiter.len(), delimiter) {
                let inner = &rest[delimiter.len()..close];
                match style {
                    Some(style) => {
                        inline(rich, inner, names, guild);
                        rich.span(start, style.clone());
                    }
                    None => {
                        rich.text.push_str(inner.trim());
                        rich.span(start, Style::Code);
                    }
                }
                at += close + delimiter.len();
                continue 'scan;
            }
        }
        match first {
            '`' => {
                if let Some(close) = closing(rest, 1, "`") {
                    rich.text.push_str(&rest[1..close]);
                    rich.span(start, Style::Code);
                    at += close + 1;
                    continue;
                }
            }
            '*' | '_' if !(first == '_' && after_word) => {
                let delimiter = &rest[..1];
                let close = closing(rest, 1, delimiter).filter(|close| {
                    let inner = &rest[1..*close];
                    let word_follows = rest[close + 1..].chars().next().is_some_and(char::is_alphanumeric);
                    !inner.starts_with(char::is_whitespace)
                        && !inner.ends_with(char::is_whitespace)
                        && !(first == '_' && word_follows)
                });
                if let Some(close) = close {
                    inline(rich, &rest[1..close], names, guild);
                    rich.span(start, Style::Italic);
                    at += close + 1;
                    continue;
                }
            }
            '[' => {
                // A masked link: [words](https://example.com).
                let masked = closing(rest, 1, "](").and_then(|label_end| {
                    let target = &rest[label_end + 2..];
                    let target_end = target.find(')')?;
                    let url = target[..target_end].trim_matches(|c| c == '<' || c == '>');
                    let url = url.split_whitespace().next()?;
                    (url.starts_with("https://") || url.starts_with("http://"))
                        .then(|| (label_end, url.to_owned(), label_end + 2 + target_end + 1))
                });
                if let Some((label_end, url, taken)) = masked {
                    if !discord_channel_link(rich, &url, Some(&rest[1..label_end]), names, guild) {
                        inline(rich, &rest[1..label_end], names, guild);
                        rich.span(start, Style::Link { url });
                    }
                    at += taken;
                    continue;
                }
            }
            '<' => {
                if let Some(taken) = angle(rich, rest, names, guild) {
                    at += taken;
                    continue;
                }
            }
            '@' => {
                let everyone = ["@everyone", "@here"].into_iter().find(|word| rest.starts_with(word));
                if let Some(word) = everyone {
                    rich.text.push_str(word);
                    let id = word[1..].to_owned();
                    rich.span(start, Style::Mention { kind: MentionKind::Everyone, id });
                    at += word.len();
                    continue;
                }
            }
            'h' if !after_word && (rest.starts_with("https://") || rest.starts_with("http://")) => {
                let end = bare_link(rest);
                if end > "https://".len() {
                    let url = &rest[..end];
                    if !discord_channel_link(rich, url, None, names, guild) {
                        rich.text.push_str(url);
                        rich.span(start, Style::Link { url: url.to_owned() });
                    }
                    at += end;
                    continue;
                }
            }
            _ => {}
        }
        rich.text.push(first);
        at += first.len_utf8();
    }
}

/// A list marker at the start of a line: how deep, whether numbered, and where the item begins.
fn list_item(line: &str) -> Option<(u8, bool, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let depth = (indent / 2).min(8) as u8;
    if rest.starts_with("- ") || rest.starts_with("* ") {
        return Some((depth, false, indent + 2));
    }
    let number = rest.find(|c: char| !c.is_ascii_digit()).filter(|end| *end > 0)?;
    rest[number..].starts_with(". ").then_some((depth, true, indent + number + 2))
}

/// The lines outside code blocks: quotes, headings, lists, then inline markdown.
fn lines(rich: &mut Rich, text: &str, names: &dyn Names, guild: Option<&str>) {
    let mut quoting = false;
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            rich.text.push('\n');
        }
        let start = rich.text.len();
        let mut line = line;
        let mut quoted = quoting;
        if !quoting {
            if let Some(rest) = line.strip_prefix(">>> ") {
                // Everything from here to the end of the message is quoted.
                (quoting, quoted, line) = (true, true, rest);
            } else if let Some(rest) = line.strip_prefix("> ") {
                (quoted, line) = (true, rest);
            }
        }
        let heading = [("### ", 3u8), ("## ", 2), ("# ", 1)]
            .into_iter()
            .find_map(|(mark, level)| line.strip_prefix(mark).map(|body| (body, level)));
        if let Some((heading, level)) = heading {
            inline(rich, heading, names, guild);
            rich.span(start, Style::Heading { level });
        } else if let Some(small) = line.strip_prefix("-# ") {
            inline(rich, small, names, guild);
            rich.span(start, Style::Small);
        } else if let Some((depth, ordered, begins)) = list_item(line) {
            // The marker stays in the text for numbered items, so the numbers are not lost.
            if ordered {
                rich.text.push_str(line[..begins].trim_start());
            }
            inline(rich, &line[begins..], names, guild);
            rich.span(start, Style::ListItem { ordered, depth });
        } else {
            inline(rich, line, names, guild);
        }
        if quoted {
            rich.span(start, Style::Quote);
        }
    }
}

/// Discord markdown into plain text and styled ranges.
pub fn markdown(rich: &mut Rich, text: &str, names: &dyn Names) {
    markdown_in(rich, text, names, None);
}

/// Discord markdown into plain text and styled ranges, with optional guild context for
/// server navigation tokens (`<id:browse>`, `<id:customize>`, `<id:guide>`).
pub fn markdown_in(rich: &mut Rich, text: &str, names: &dyn Names, guild: Option<&str>) {
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let Some(close) = closing(rest, open + 3, "```") else {
            break;
        };
        let before = rest[..open].strip_suffix('\n').unwrap_or(&rest[..open]);
        if !before.is_empty() {
            lines(rich, before, names, guild);
        }
        if !rich.text.is_empty() {
            rich.text.push('\n');
        }
        let code = &rest[open + 3..close];
        // A first line of letters alone names the language.
        let (language, code) = match code.split_once('\n') {
            Some((first, body))
                if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphanumeric() || "+-#_.".contains(c)) =>
            {
                (first, body)
            }
            _ => ("", code.strip_prefix('\n').unwrap_or(code)),
        };
        let start = rich.text.len();
        rich.text.push_str(code.strip_suffix('\n').unwrap_or(code));
        rich.span(start, Style::CodeBlock { language: language.to_owned() });
        rest = &rest[close + 3..];
        if let Some(after) = rest.strip_prefix('\n') {
            rest = after;
        }
        if !rest.is_empty() {
            rich.text.push('\n');
        }
    }
    if !rest.is_empty() {
        lines(rich, rest, names, guild);
    }
}

fn media_kind(mime: &str, name: &str) -> MediaKind {
    let lower = name.to_ascii_lowercase();
    match mime {
        "image/gif" => MediaKind::Gif,
        mime if mime.starts_with("image/") => MediaKind::Image,
        mime if mime.starts_with("video/") => MediaKind::Video,
        mime if mime.starts_with("audio/") => MediaKind::Audio,
        "" if lower.ends_with(".gif") => MediaKind::Gif,
        "" if [".png", ".jpg", ".jpeg", ".webp"].iter().any(|end| lower.ends_with(end)) => MediaKind::Image,
        _ => MediaKind::File,
    }
}

/// The size a picture is drawn at on its own: inside 400 x 300 points, never larger than it is,
/// keeping the shape (`docs/design/03-conversation.md`).
fn fitted(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 || (width <= 400 && height <= 300) {
        return (width, height);
    }
    // Whichever side is further over its limit decides.
    if u64::from(width) * 300 >= u64::from(height) * 400 {
        (400, ((u64::from(height) * 400) / u64::from(width)).max(1) as u32)
    } else {
        (((u64::from(width) * 300) / u64::from(height)).max(1) as u32, 300)
    }
}

/// Discord's media proxy resizes when asked, and answers a still for a video or a GIF. A GIF
/// asked for `animated` comes back as an animated WebP at that size, so a row plays it without
/// the whole original. The parameter names are from the community documentation and
/// unconfirmed against a real account.
fn resized(proxy: &str, width: u32, height: u32, animated: bool) -> String {
    if width == 0 || height == 0 {
        return proxy.to_owned();
    }
    let joiner = if proxy.contains('?') { '&' } else { '?' };
    let moving = if animated { "&animated=true" } else { "" };
    format!("{proxy}{joiner}format=webp&width={width}&height={height}{moving}")
}

/// Whether a proxied picture may move: a GIF, or a WebP, which can.
fn may_move(address: &str) -> bool {
    let path = address.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    path.ends_with(".gif") || path.ends_with(".webp")
}

/// The larger sizes of a picture registered at `fitted`: one and a half, two and three times
/// it, for a 2x screen and for a grid tile, which crops and so draws the whole picture larger
/// than its box. Never larger than the picture is; the picture itself is the last size when
/// it is smaller than the next step.
fn larger(proxy: &str, (width, height): (u32, u32), (full_width, full_height): (u32, u32), animated: bool) -> Vec<(u32, String)> {
    let mut sizes = Vec::new();
    for (times, over) in [(3, 2), (2, 1), (3, 1)] {
        let (wide, high) = (width * times / over, height * times / over);
        if wide >= full_width || high >= full_height {
            if full_width > width {
                sizes.push((full_width.max(full_height), resized(proxy, full_width, full_height, animated)));
            }
            break;
        }
        sizes.push((wide.max(high), resized(proxy, wide, high, animated)));
    }
    sizes
}

/// A voice message's bars: base64 of one byte each.
fn waveform(encoded: Option<&str>) -> Vec<u8> {
    let decoded = encoded.and_then(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded.trim()).ok());
    let mut bars = decoded.unwrap_or_default();
    bars.truncate(protocol::WAVEFORM_LIMIT);
    bars
}

fn attachment(rich: &mut Rich, attached: &Attached, blur_explicit: bool) -> Attachment {
    let mime = attached.content_type.as_deref().unwrap_or("");
    let mime = mime.split(';').next().unwrap_or("").trim();
    let kind = media_kind(mime, &attached.filename);
    let file = MediaFile {
        width: attached.width,
        height: attached.height,
        bytes: attached.size,
        ..MediaFile::new(format!("att:{}", attached.id), kind)
    };
    rich.files.push((file.clone(), attached.url.clone()));
    let drawn = matches!(kind, MediaKind::Image | MediaKind::Gif | MediaKind::Video);
    let thumb = attached.proxy_url.as_deref().filter(|_| drawn && attached.width > 0).map(|proxy| {
        let (width, height) = fitted(attached.width, attached.height);
        let thumb = MediaFile {
            width,
            height,
            ..MediaFile::new(format!("att:{}:thumb", attached.id), MediaKind::Image)
        };
        let animated = kind == MediaKind::Gif || mime == "image/webp";
        rich.files.push((thumb.clone(), resized(proxy, width, height, animated)));
        let sizes = larger(proxy, (width, height), (attached.width, attached.height), animated);
        if !sizes.is_empty() {
            rich.files.sizes.push((thumb.id.clone(), sizes));
        }
        thumb
    });
    // A title the sender gave the file is the name shown; a blank one is no title.
    let name = attached.title.as_deref().map(str::trim).filter(|title| !title.is_empty()).unwrap_or(&attached.filename);
    Attachment {
        alt: protocol::clip_alt(attached.description.as_deref().unwrap_or("")),
        spoiler: attached.spoiler() || (blur_explicit && attached.is_explicit()),
        sticker: false,
        waveform: waveform(attached.waveform.as_deref()),
        name: name.to_owned(),
        file,
        thumb,
        preview: None,
        mime: mime.to_owned(),
        duration: attached.duration_secs.round() as u32,
        round: false,
        view_once: false,
        sticker_set: String::new(),
        page: String::new(),
        transcription: None,
        transcript: None,
    }
}

/// A sticker is drawn like an attached picture; the shell renders Lottie JSON with tlottie.
fn sticker(rich: &mut Rich, sticker: &model::Sticker) -> Attachment {
    let (kind, url, mime) = match sticker.format_type {
        3 => (MediaKind::File, format!("{}/stickers/{}.json", model::cdn(), sticker.id), "application/json"),
        4 => (MediaKind::Gif, format!("{}/stickers/{}.gif", model::media(), sticker.id), "image/gif"),
        2 => (MediaKind::Image, model::apng_sticker(&sticker.id), "image/png"),
        _ => (MediaKind::Image, format!("{}/stickers/{}.png?size=160", model::media(), sticker.id), "image/png"),
    };
    let file = MediaFile {
        width: 160,
        height: 160,
        ..MediaFile::new(format!("sticker:{}", sticker.id), kind)
    };
    // An APNG's frames are passed through at the size Discord made them.
    if kind == MediaKind::Image && sticker.format_type != 2 {
        let twice = format!("{}/stickers/{}.png?size=320", model::media(), sticker.id);
        rich.files.sizes.push((file.id.clone(), vec![(320, twice)]));
    }
    rich.files.push((file.clone(), url));
    Attachment {
        alt: String::new(),
        spoiler: false,
        sticker: true,
        waveform: Vec::new(),
        name: sticker.name.clone(),
        file,
        thumb: None,
        preview: None,
        mime: mime.to_owned(),
        duration: 0,
        round: false,
        view_once: false,
        sticker_set: String::new(),
        page: String::new(),
        transcription: None,
        transcript: None,
    }
}

/// An embed's picture, registered at the size a row draws it, when Discord's proxy serves it.
fn embed_still(rich: &mut Rich, id: String, picture: &EmbedImage) -> Option<MediaFile> {
    let proxy = picture.proxy_url.as_deref()?;
    let (width, height) = fitted(picture.width, picture.height);
    let file = MediaFile { width, height, ..MediaFile::new(id, MediaKind::Image) };
    let animated = may_move(proxy);
    rich.files.push((file.clone(), resized(proxy, width, height, animated)));
    let sizes = larger(proxy, (width, height), (picture.width, picture.height), animated);
    if !sizes.is_empty() {
        rich.files.sizes.push((file.id.clone(), sizes));
    }
    Some(file)
}

/// A GIF from Tenor or Giphy (`gifv`, an MP4) or a video Discord's proxy serves, as an
/// attachment: its still in the row, the moving picture fetched only when it is played. A
/// video only its site can play (YouTube) has no proxy address and stays a link card.
fn moving(rich: &mut Rich, message: &str, index: usize, embed: &Embed, blur_explicit: bool) -> Option<Attachment> {
    let kind = match embed.kind.as_deref() {
        Some("gifv") => MediaKind::Gif,
        Some("video") => MediaKind::Video,
        _ => return None,
    };
    let video = embed.video.as_ref()?;
    let proxy = video.proxy_url.as_deref()?;
    let poster = embed.thumbnail.as_ref().or(embed.image.as_ref())?;
    let thumb = embed_still(rich, format!("embed:{message}:{index}"), poster)?;
    let file = MediaFile {
        width: video.width,
        height: video.height,
        ..MediaFile::new(format!("embed:{message}:{index}:video"), kind)
    };
    rich.files.push((file.clone(), proxy.to_owned()));
    let name = match kind {
        MediaKind::Gif => "GIF".to_owned(),
        _ => embed.title.clone().filter(|title| !title.is_empty()).unwrap_or_else(|| "Video".to_owned()),
    };
    let mut attachment = Attachment::new(name, file);
    attachment.thumb = Some(thumb);
    attachment.mime = "video/mp4".to_owned();
    attachment.spoiler = blur_explicit && embed.is_explicit();
    attachment.duration = whole_seconds(video.duration_secs);
    if let Some(url) = embed.url.as_deref().filter(|url| url.starts_with("https://") || url.starts_with("http://")) {
        attachment.page = url.to_owned();
    }
    Some(attachment)
}

/// A card, with a picture only when Discord's own proxy serves it.
fn preview(rich: &mut Rich, message: &str, index: usize, embed: &Embed, blur_explicit: bool) -> Preview {
    let mut description = embed.description.clone().unwrap_or_default();
    for field in &embed.fields {
        if !description.is_empty() {
            description.push('\n');
        }
        description.push_str(&format!("{}: {}", field.name, field.value));
    }
    let picture = (!blur_explicit || !embed.is_explicit())
        .then(|| embed.image.as_ref().or(embed.thumbnail.as_ref()))
        .flatten();
    let image = picture.and_then(|picture| embed_still(rich, format!("embed:{message}:{index}"), picture));
    let site = embed.provider.as_ref().or(embed.author.as_ref());
    // No proxy address: YouTube and the other site players stay a card. The poster is large
    // in the row; a click opens the browser. This client has no web view to play them in.
    let elsewhere = matches!(embed.kind.as_deref(), Some("video") | Some("gifv"));
    let duration = embed.video.as_ref().filter(|_| elsewhere).map(|video| whole_seconds(video.duration_secs)).unwrap_or(0);
    Preview {
        url: embed.url.clone().unwrap_or_default(),
        site: site.map_or_else(String::new, |site| site.name.clone()),
        title: embed.title.clone().unwrap_or_default(),
        description,
        image,
        duration,
        opens_in_browser: elsewhere,
    }
}

fn whole_seconds(value: f64) -> u32 {
    value.round().clamp(0.0, u32::MAX as f64) as u32
}

/// A small picture of an embed (an author's or footer's icon), from Discord's proxy only.
fn embed_icon(rich: &mut Rich, id: String, proxy: Option<&str>) -> Option<MediaFile> {
    let proxy = proxy.filter(|proxy| !proxy.is_empty())?;
    let file = MediaFile { width: 40, height: 40, ..MediaFile::new(id, MediaKind::Image) };
    rich.files.push((file.clone(), resized(proxy, 40, 40, false)));
    Some(file)
}

/// What a bot or webhook composed, as a framed card. Its description keeps its styles and
/// links; a title, a field and a footer are plain: the markup goes, the words stay. Pictures
/// come from Discord's proxy or not at all.
fn card(rich: &mut Rich, message: &str, index: usize, embed: &Embed, names: &dyn Names, blur_explicit: bool) -> Card {
    let plain = |text: &str| {
        let mut rich = Rich::default();
        markdown(&mut rich, text, names);
        rich.text
    };
    let mut said = Rich::default();
    markdown(&mut said, embed.description.as_deref().unwrap_or(""), names);
    // A custom emoji in the description is fetched like one in a message.
    rich.files.merge(std::mem::take(&mut said.files));
    let link = |url: Option<&String>| url.filter(|url| is_safe_http_url(url)).cloned().unwrap_or_default();
    let author = embed.author.as_ref();
    let author_name = author.map_or("", |author| author.name.as_str());
    // With no title, the author's name is the title, as before; it is not said twice.
    let title = embed.title.as_deref().filter(|title| !title.is_empty());
    let shown = !blur_explicit || !embed.is_explicit();
    let thumbnail = embed.thumbnail.as_ref().filter(|_| shown);
    Card {
        author: title.map_or_else(String::new, |_| plain(author_name)),
        author_link: link(author.and_then(|author| author.url.as_ref())),
        author_icon: author.and_then(|author| embed_icon(rich, format!("embed:{message}:{index}:author"), author.proxy_icon_url.as_deref())),
        title: plain(title.unwrap_or(author_name)),
        title_link: link(embed.url.as_ref()),
        text: said.text,
        text_spans: said.spans,
        fields: embed.fields.iter().map(|field| (plain(&field.name), plain(&field.value))).collect(),
        // Nothing to say when every field shares its row: that is what a card does anyway.
        inline: match embed.fields.iter().all(|field| field.inline) {
            true => Vec::new(),
            false => embed.fields.iter().map(|field| field.inline).collect(),
        },
        footer: embed.footer.as_ref().map_or_else(String::new, |footer| footer.text.clone()),
        footer_icon: embed.footer.as_ref().and_then(|footer| embed_icon(rich, format!("embed:{message}:{index}:footer"), footer.proxy_icon_url.as_deref())),
        time: embed.timestamp.as_deref().and_then(model::iso_seconds),
        thumbnail: thumbnail.and_then(|picture| embed_still(rich, format!("embed:{message}:{index}:thumb"), picture)),
        color: embed.color,
        sponsored: false,
        ..Card::default()
    }
}

fn reactions(rich: &mut Rich, reacted: &[model::Reacted]) -> Vec<Reaction> {
    let mapped = reacted.iter().filter_map(|reaction| {
        let (normal, burst) = match &reaction.count_details {
            Some(details) => (details.normal, details.burst),
            None => (reaction.count, 0),
        };
        if normal == 0 && burst == 0 {
            return None;
        }
        let (emoji, file) = reaction.emoji.spelled();
        let file = file.map(|(file, url)| {
            rich.files.push((file.clone(), url));
            file
        });
        Some(Reaction {
            emoji,
            file,
            count: normal,
            mine: reaction.me,
            // Discord's message payload counts reactions and never names who: the list
            // would need one request per emoji, which this build does not make.
            who: Vec::new(),
            burst,
            mine_burst: reaction.me_burst,
            burst_colors: reaction.burst_colors.clone(),
        })
    });
    mapped.collect()
}

/// The message as one line of plain text, for a reply's quote.
fn snippet(posted: &Posted, names: &dyn Names) -> String {
    let mut rich = Rich::default();
    markdown(&mut rich, posted.content.as_deref().unwrap_or(""), names);
    let line = rich.text.replace('\n', " ");
    if !line.is_empty() {
        return line.chars().take(120).collect();
    }
    let attached = posted.attachments.as_deref().and_then(|attached| attached.first());
    attached.map_or_else(String::new, |attached| attached.filename.clone())
}

pub fn sender_name(posted: &Posted) -> String {
    let nick = posted.member.as_ref().and_then(|member| member.nick.as_deref()).filter(|nick| !nick.is_empty());
    let own = posted.author.as_ref().map_or("", |author| author.name());
    nick.unwrap_or(own).to_owned()
}

/// Formats a server boost line (types 8–11), preserving the boost count in `content` and any
/// tier level reached.
fn boost_line(kind: u8, who: &str, content: &str, server: &str) -> String {
    let count = content.trim().parse::<u32>().ok().filter(|&n| n > 1);
    let tier = match kind {
        9 => Some(1),
        10 => Some(2),
        11 => Some(3),
        _ => None,
    };
    let head = match (count, tier) {
        (Some(n), _) => format!("{who} boosted the server {n} times!"),
        (None, Some(_)) => format!("{who} boosted the server!"),
        (None, None) => format!("{who} boosted the server."),
    };
    match tier {
        Some(level) => format!("{head} {server} has reached Level {level}."),
        None => head,
    }
}

/// The one grey line a system message is. `None` for types that are ordinary messages.
fn system_line(posted: &Posted, kind: u8, who: &str, content: &str, mentioned: &str, server: &str) -> Option<String> {
    Some(match kind {
        0 | 19 | 20 | 23 => return None,
        1 => format!("{who} added {mentioned} to the group."),
        2 if mentioned == who || mentioned.is_empty() => format!("{who} left the group."),
        2 => format!("{who} removed {mentioned} from the group."),
        3 => format!("{who} started a call."),
        4 => format!("{who} changed the group name: {content}"),
        5 => format!("{who} changed the group icon."),
        6 => format!("{who} pinned a message."),
        7 => format!("{who} joined the server."),
        8..=11 => boost_line(kind, who, content, server),
        12 => format!("{who} added {content} to this channel."),
        14 => "This server has been removed from Server Discovery because it no longer passes all the requirements.".to_owned(),
        15 => "This server is eligible for Server Discovery again and has been automatically relisted!".to_owned(),
        16 => "This server has failed Discovery activity requirements for 1 week. If this server fails for 4 weeks in a row, it will be automatically removed from Discovery.".to_owned(),
        17 => "This server has failed Discovery activity requirements for 3 weeks in a row. If this server fails for 1 more week, it will be automatically removed from Discovery.".to_owned(),
        18 => format!("{who} started a thread: {content}"),
        21 => format!("{who} started a thread."),
        22 => "Sending invites to your server is the best way to grow your community.".to_owned(),
        25 => match posted.role_subscription_data.as_ref() {
            Some(sub) if !sub.tier_name.is_empty() && sub.is_renewal && sub.total_months_subscribed > 1 => {
                format!(
                    "{who} renewed {} and has been a subscriber of {server} for {} months!",
                    sub.tier_name, sub.total_months_subscribed
                )
            }
            Some(sub) if !sub.tier_name.is_empty() => {
                format!("{who} joined {} and became a subscriber of {server}!", sub.tier_name)
            }
            _ => format!("{who} subscribed to {server}."),
        },
        26 => {
            if content.is_empty() {
                "Upgrade to unlock more features for this app.".to_owned()
            } else {
                content.to_owned()
            }
        }
        27 => format!("{who} started {content}"),
        28 => format!("{who} ended {content}"),
        29 => format!("{who} is now a speaker."),
        30 => format!("{who} requested to speak."),
        31 => format!("{who} changed the Stage topic: {content}"),
        32 => {
            if content.is_empty() {
                format!("{who} subscribed to an app for {server}.")
            } else {
                format!("{who} subscribed to {content} for {server}.")
            }
        }
        36 => format!("{who} enabled security actions for this server."),
        37 => format!("{who} disabled security actions for this server."),
        38 => format!("{who} reported a raid in this server."),
        39 => format!("{who} marked the raid alert as a false alarm."),
        44 => {
            let product = posted
                .purchase_notification
                .as_ref()
                .and_then(|p| p.guild_product_purchase.as_ref())
                .map(|p| p.product_name.as_str())
                .filter(|name| !name.is_empty())
                .or_else(|| (!content.is_empty()).then_some(content));
            match product {
                Some(name) => format!("{who} has purchased {name}!"),
                None => format!("{who} made a purchase."),
            }
        }
        46 => {
            let winner = posted.embeds.as_deref().unwrap_or_default().iter().find(|embed| embed.kind.as_deref() == Some("poll_result"))
                .and_then(|embed| embed_field(embed, "victor_answer_text").or_else(|| embed_field(embed, "victor_answer_emoji_name")));
            winner.map_or_else(|| "A poll has closed.".to_owned(), |winner| format!("A poll has closed. {winner} won."))
        }
        67 => format!("{who} accepted your friend request."),
        _ => {
            tracing::debug!(message_type = kind, "chatd: Discord: unsupported system message type");
            return Some(String::new());
        }
    })
}

fn embed_field(embed: &Embed, name: &str) -> Option<String> {
    embed
        .fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn push_quote(line: &mut String, seen: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() || seen.iter().any(|have| have == text) {
        return;
    }
    seen.push(text.to_owned());
    line.push('\n');
    line.push('"');
    line.push_str(text);
    line.push('"');
}

/// Type 24, from the alert embed. Missing pieces drop out of the sentence.
fn automod_alert(who: &str, posted: &Posted, names: &dyn Names) -> String {
    let Some(embed) = posted.embeds.as_deref().and_then(|embeds| embeds.first()) else {
        return "AutoMod flagged a message.".to_owned();
    };
    let name = if who.is_empty() { "someone" } else { who };
    let mut line = format!("AutoMod blocked a message from {name}");
    if let Some(id) = embed_field(embed, "channel_id") {
        let title = names.channel(&id).filter(|title| !title.is_empty()).unwrap_or(id);
        line.push_str(" in #");
        line.push_str(title.trim_start_matches('#'));
    }
    if let Some(rule) = embed_field(embed, "rule_name") {
        line.push_str(" — rule ");
        line.push_str(&rule);
    }
    let mut seen = Vec::new();
    if let Some(text) = embed.description.as_deref() {
        push_quote(&mut line, &mut seen, text);
    }
    if let Some(text) = embed_field(embed, "keyword_matched_content") {
        push_quote(&mut line, &mut seen, &text);
    }
    if let Some(text) = embed_field(embed, "keyword") {
        push_quote(&mut line, &mut seen, &text);
    }
    line
}
/// Extracts Discord server invite links (`discord.gg/{code}`, `discord.com/invite/{code}`,
/// `discordapp.com/invite/{code}`, with optional `?event={id}`) and scheduled event links
/// (`discord.com/events/{guild}/{event}`) from translated link spans. No network calls are made.
pub fn extract_invites(spans: &[Span]) -> Vec<MessageInvite> {
    let mut found: Vec<MessageInvite> = Vec::new();
    for span in spans {
        if found.len() >= protocol::MESSAGE_INVITES_LIMIT {
            break;
        }
        let Style::Link { url } = &span.style else {
            continue;
        };
        let Some(invite) = parse_invite_or_event_url(url) else {
            continue;
        };
        let dup = found.iter().any(|have| {
            have.code == invite.code
                && have.event.as_ref().map(|e| (&e.guild_id, &e.id))
                    == invite.event.as_ref().map(|e| (&e.guild_id, &e.id))
        });
        if !dup {
            found.push(invite);
        }
    }
    found
}

fn parse_invite_or_event_url(raw: &str) -> Option<MessageInvite> {
    let stripped = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;
    let (without_frag, _) = stripped.split_once('#').unwrap_or((stripped, ""));
    let (host_and_path, query) = without_frag.split_once('?').unwrap_or((without_frag, ""));
    let (host, path) = host_and_path.split_once('/')?;
    let host_lower = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    let event_param = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("event="))
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned);

    let valid_code = |code: &str| {
        !code.is_empty()
            && code.len() <= 64
            && code.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    };

    if host_lower == "discord.gg" || host_lower == "www.discord.gg" {
        let code = path.split('/').next().unwrap_or("");
        if !valid_code(code) {
            return None;
        }
        let canonical = match &event_param {
            Some(ev) => format!("https://discord.gg/{code}?event={ev}"),
            None => format!("https://discord.gg/{code}"),
        };
        return Some(MessageInvite {
            code: code.to_owned(),
            url: canonical,
            state: InviteState::Unresolved,
            event: event_param.map(|id| ScheduledEvent { id, ..ScheduledEvent::default() }),
            ..MessageInvite::default()
        });
    }

    if matches!(
        host_lower.as_str(),
        "discord.com" | "www.discord.com" | "discordapp.com" | "www.discordapp.com" | "ptb.discord.com" | "canary.discord.com"
    ) {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.first() == Some(&"invite") {
            let code = *segments.get(1)?;
            if !valid_code(code) {
                return None;
            }
            let canonical = match &event_param {
                Some(ev) => format!("https://discord.gg/{code}?event={ev}"),
                None => format!("https://discord.gg/{code}"),
            };
            return Some(MessageInvite {
                code: code.to_owned(),
                url: canonical,
                state: InviteState::Unresolved,
                event: event_param.map(|id| ScheduledEvent { id, ..ScheduledEvent::default() }),
                ..MessageInvite::default()
            });
        }
        if segments.first() == Some(&"events") {
            let guild_id = *segments.get(1)?;
            let event_id = *segments.get(2)?;
            if !digits(guild_id) || !digits(event_id) {
                return None;
            }
            return Some(MessageInvite {
                code: String::new(),
                url: format!("https://discord.com/events/{guild_id}/{event_id}"),
                space_id: Some(guild_id.to_owned()),
                state: InviteState::Unresolved,
                event: Some(ScheduledEvent {
                    id: event_id.to_owned(),
                    guild_id: guild_id.to_owned(),
                    ..ScheduledEvent::default()
                }),
                ..MessageInvite::default()
            });
        }
    }
    None
}

/// Where the official app shows this message.
pub fn jump_link(guild: Option<&str>, channel: &str, message: &str) -> String {
    format!("https://discord.com/channels/{}/{channel}/{message}", guild.unwrap_or("@me"))
}

/// Whose message this is and where, which the payload alone does not always say.
pub struct Place<'a> {
    pub account: &'a AccountKey,
    pub me: &'a str,
    pub guild: Option<&'a str>,
    pub names: &'a dyn Names,
    /// What the author goes by here when it is not what the message says: a friend's nickname.
    pub author: Option<String>,
    /// Whether explicitly flagged media (`CONTAINS_EXPLICIT_MEDIA`) should be blurred behind a spoiler cover.
    pub blur_explicit: bool,
}

fn build_call_note(
    payload: Option<&model::CallPayload>,
    caller_id: &str,
    who: &str,
    sent: i64,
    outgoing: bool,
    me: &str,
) -> (protocol::CallNote, Option<String>) {
    let (outcome, seconds, participants, text) = match payload {
        Some(payload) => match payload.ended_timestamp.as_deref().and_then(model::iso_seconds) {
            Some(ended) => {
                let dur = u32::try_from((ended - sent).max(0)).ok().filter(|&s| s > 0);
                // Missed: call ended, I did not start it, and I am not in `participants`.
                let missed = !outgoing && !payload.participants.iter().any(|id| id == me);
                let outcome = if missed { protocol::CallOutcome::Missed } else { protocol::CallOutcome::Ended };
                let text = if missed {
                    format!("Missed call from {who}")
                } else if let Some(secs) = dur {
                    let mins = secs / 60;
                    if mins >= 60 {
                        format!("Call with {who} · {} hr {} min", mins / 60, mins % 60)
                    } else if mins >= 1 {
                        format!("Call with {who} · {mins} min")
                    } else {
                        format!("Call with {who} · {secs} sec")
                    }
                } else {
                    format!("Call with {who}")
                };
                (outcome, dur, payload.participants.clone(), Some(text))
            }
            None => (
                protocol::CallOutcome::Live,
                None,
                payload.participants.clone(),
                Some("Call in progress".to_owned()),
            ),
        },
        None => (protocol::CallOutcome::Ended, None, Vec::new(), None),
    };
    (
        protocol::CallNote {
            outcome,
            caller_id: caller_id.to_owned(),
            seconds,
            participants,
            video: false,
        },
        text,
    )
}

/// A whole message object into the model, with every file it refers to. `mentions_me` is left
/// false: it depends on the user's settings, which the caller knows.
pub fn message(posted: &Posted, place: &Place) -> (Message, Files) {
    let mut rich = Rich::default();
    let author = posted.author.clone().unwrap_or_default();
    let who = place.author.clone().unwrap_or_else(|| sender_name(posted));
    let content = posted.content.as_deref().unwrap_or("");
    let kind = posted.kind.unwrap_or(0);
    let link = jump_link(place.guild, &posted.channel_id, &posted.id);
    let flags = posted.flags.unwrap_or(0);

    let has_snapshots = !posted.message_snapshots.is_empty();
    let mut message_kind = MessageKind::Normal;
    let mentioned = posted.mentions.first().map_or("", |user| user.name());
    let server = place
        .guild
        .and_then(|guild| place.names.space(guild))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This server".to_owned());
    if kind == 24 {
        // Posted by the member AutoMod caught. The row is the alert, not their message.
        rich.text = automod_alert(&who, posted, place.names);
        message_kind = MessageKind::System;
    } else {
        match system_line(posted, kind, &who, content, mentioned, &server) {
            None => {
                markdown_in(&mut rich, content, place.names, place.guild);
                // What was forwarded is the message's own text; a forward has none of its own.
                for snapshot in &posted.message_snapshots {
                    let snap_text = snapshot.message.content.as_deref().unwrap_or("");
                    if !snap_text.is_empty() {
                        if !rich.text.is_empty() {
                            rich.text.push('\n');
                        }
                        markdown_in(&mut rich, snap_text, place.names, place.guild);
                    }
                }
            }
            Some(line) if line.is_empty() => {
                rich.text = if content.is_empty() { "A message tinyclient cannot show.".to_owned() } else { content.to_owned() };
                message_kind = MessageKind::Unsupported { link: link.clone() };
            }
            Some(line) => {
                rich.text = line;
                message_kind = MessageKind::System;
            }
        }
    }

    let carried: Vec<&Posted> = if has_snapshots {
        posted.message_snapshots.iter().map(|snapshot| &*snapshot.message).collect()
    } else {
        vec![posted]
    };
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut embeds: Vec<model::Embed> = Vec::new();
    let mut composed = false;
    for part in &carried {
        for attached in part.attachments.as_deref().unwrap_or(&[]) {
            attachments.push(attachment(&mut rich, attached, place.blur_explicit));
        }
        attachments.extend(part.sticker_items.iter().map(|item| sticker(&mut rich, item)));
        // The alert's embed is the system line. A preview would glue those fields back on.
        if kind != 24 && kind != 46 {
            embeds.extend(part.embeds.as_deref().unwrap_or(&[]).iter().cloned());
        }
        composed |= part.author.as_ref().is_some_and(|author| author.bot) || part.webhook_id.is_some();
    }
    let (mut composed_cards, previews, moving_pictures) = cards(&mut rich, &posted.id, &embeds, place.names, composed, place.blur_explicit);
    let card = (!composed_cards.is_empty()).then(|| Box::new(composed_cards.remove(0)));
    attachments.extend(moving_pictures);
    if flags & model::FLAG_IS_VOICE_MESSAGE != 0 {
        for attachment in &mut attachments {
            if attachment.file.kind == MediaKind::File {
                attachment.file.kind = MediaKind::Audio;
            }
        }
    }
    let reactions = reactions(&mut rich, posted.reactions.as_deref().unwrap_or(&[]));
    let app_id = posted.application_id.as_deref().unwrap_or(&author.id);
    let (controls, _dropped_components) = match carried.first().and_then(|part| part.components.as_deref()) {
        Some(comps) if kind != 24 && !comps.is_empty() => extract_components(
            comps,
            &posted.id,
            app_id,
            flags,
            &link,
            place.names,
            &mut rich,
            &mut attachments,
        ),
        _ => (Vec::new(), false),
    };
    let origin = posted.message_reference.as_ref().filter(|reference| reference.kind == 1 && has_snapshots);
    let mut forwarded_from = origin.map(|origin| Box::new(referenced(origin, place)));
    if forwarded_from.is_none() && flags & model::FLAG_IS_CROSSPOST != 0 {
        forwarded_from = posted.message_reference.as_ref().map(|origin| Box::new(referenced(origin, place)));
    }

    let poll = posted.poll.as_ref().and_then(|raw| {
        let (poll, files) = super::polls::convert(raw, None);
        rich.files.files.extend(files);
        poll.map(Box::new)
    });
    if poll.is_none() {
        if let Some(question) = posted.poll.as_ref().and_then(|poll| poll.question.as_ref()).and_then(|question| question.text.as_deref()) {
            rich.text = format!("Poll: {question}");
            rich.spans.clear();
            message_kind = MessageKind::Unsupported { link: link.clone() };
        }
    }
    if poll.is_none() && message_kind == MessageKind::Normal
        && rich.text.is_empty()
        && attachments.is_empty()
        && previews.is_empty()
        && card.is_none()
        && controls.is_empty()
    {
        if flags & model::FLAG_LOADING != 0 {
            rich.text = format!("{who} is thinking…");
        } else {
            // Buttons, menus, or something newer than this build: never a blank row.
            let has_components = posted.components.as_deref().is_some_and(|c| !c.is_empty());
            rich.text = if !has_components {
                "A message tinyclient cannot show.".to_owned()
            } else {
                "Interactive message".to_owned()
            };
            message_kind = MessageKind::Unsupported { link: link.clone() };
        }
    }

    let reply_to = match (&posted.message_reference, &posted.referenced_message) {
        (Some(reference), Some(original)) if reference.kind == 0 => Some(ReplyTo {
            deleted: false,
            // A webhook's author id names the webhook, not a person.
            person: original.author.as_ref().filter(|_| original.webhook_id.is_none()).map(|author| Key::new(place.account, &author.id)),
            id: original.id.clone(),
            sender: sender_name(original),
            snippet: snippet(original, place.names),
            thumb: None,
            story: false,
        }),
        (None, Some(original)) if kind == 21 => Some(ReplyTo {
            deleted: false,
            person: original.author.as_ref().filter(|_| original.webhook_id.is_none()).map(|author| Key::new(place.account, &author.id)),
            id: original.id.clone(),
            sender: sender_name(original),
            snippet: snippet(original, place.names),
            thumb: None,
            story: false,
        }),
        // The original was deleted, or Discord did not include it.
        (Some(reference), None) if reference.kind == 0 && matches!(kind, 19 | 21) => reference.message_id.as_ref().map(|id| ReplyTo {
            deleted: true,
            person: None,
            id: id.clone(),
            sender: String::new(),
            snippet: String::new(),
            thumb: None,
            story: false,
        }),
        _ => None,
    };
    let jump = jump_target(posted, kind);
    // The thread object names the count and the newest id. Faces and unread are filled
    // as replies arrive, and a thread update can raise the count.
    let thread = posted.thread.as_ref().map(|thread| ThreadSummary {
        participants: Vec::new(),
        unread: 0,
        id: thread.id.clone(),
        replies: thread.message_count.unwrap_or(0),
        last: thread.last_message_id.as_deref().map_or(0, seconds),
    });

    let (avatar, avatar_url) = match (
        place.guild,
        posted.member.as_ref().and_then(|member| member.avatar.as_deref()).filter(|hash| !hash.is_empty()),
    ) {
        (Some(guild), Some(hash)) if !author.id.is_empty() && posted.webhook_id.is_none() => {
            model::member_avatar(guild, &author.id, hash).unwrap_or_else(|| author.avatar())
        }
        _ => author.avatar(),
    };
    rich.files.push((avatar.clone(), avatar_url));
    let outgoing = !author.id.is_empty() && author.id == place.me;
    let sent = seconds(&posted.id);
    let call_note = if kind == 3 {
        let (note, text) = build_call_note(posted.call.as_ref(), &author.id, &who, sent, outgoing, place.me);
        if let Some(text) = text {
            rich.text = text;
        }
        Some(note)
    } else {
        None
    };
    let flags = posted.flags.unwrap_or(0);
    let published = flags & model::CROSSPOSTED != 0;
    let crosspost_source = if flags & model::IS_CROSSPOST != 0 {
        posted.message_reference.as_ref().map_or_else(String::new, |origin| {
            let space = origin.guild_id.as_deref().and_then(|g| place.names.space(g));
            let channel = origin.channel_id.as_deref().and_then(|c| place.names.channel(c)).map(|name| format!("#{name}"));
            match (space, channel) {
                (Some(s), Some(c)) => format!("{s} › {c}"),
                (Some(s), None) => s,
                (None, Some(c)) => c,
                (None, None) => "Announcement".to_owned(),
            }
        })
    } else {
        String::new()
    };
    // Our own client's nonce is the shell's client id: digits, at most 25.
    let client_id = posted.nonce.clone().filter(|nonce| outgoing && digits(nonce) && nonce.len() <= 25);
    let invites = extract_invites(&rich.spans);
    let message = Message {
        saved_history: false,
        sent_subsec_millis: 0,
        seal: protocol::Seal::Plain,
        card,
        cards: composed_cards,
        deleted: false,
        moderated: false,
        forwarded: forwarded_from,
        id: posted.id.clone(),
        sender: Sender {
            // A webhook's author id names the webhook, not a person.
            id: if posted.webhook_id.is_some() { String::new() } else { author.id.clone() },
            name: who,
            avatar: Some(avatar),
            bot: author.bot || posted.webhook_id.is_some(),
            ..Sender::default()
        },
        sent,
        seq: 0,
        expires_at: None,
        outgoing,
        text: rich.text,
        spans: rich.spans,
        attachments,
        previews,
        reactions,
        reply_to,
        thread,
        poll,
        views: None,
        edited: posted.edited_timestamp.is_some(),
        edited_at: posted.edited_timestamp.as_deref().and_then(model::iso_seconds),
        prior_text: None,
        pinned: posted.pinned.unwrap_or(false),
        pinned_by: None,
        pinned_at: None,
        starred: false,
        mentions_me: false,
        kind: message_kind,
        state: protocol::SendState::Sent,
        waiting: false,
        client_id,
        deliveries: Vec::new(),
        receipt_note: String::new(),
        call: call_note,
        can_edit: None,
        can_delete: None,
        importance: protocol::Importance::Normal,
        announcement: false,
        published,
        crosspost_source,
        effect: None,
        contacts: Vec::new(),
        sent_via: None,
        permalink: link,
        controls,
        via: command_via(posted),
        ephemeral: flags & model::FLAG_EPHEMERAL != 0,
        silent: flags & model::FLAG_SUPPRESS_NOTIFICATIONS != 0,
        loading: flags & model::FLAG_LOADING != 0,
        invites,
        buttons: Vec::new(),
        jump,
        media_unread: false,
        keyboard: None,
    };
    (message, rich.files)
}

/// A system-message jump stays in this channel. A reference or quoted message in another
/// channel is still a quote, and it is not a clickable jump.
fn jump_target(posted: &Posted, kind: u8) -> Option<String> {
    if !matches!(kind, 6 | 18 | 21 | 46) {
        return None;
    }
    let reference = posted.message_reference.as_ref();
    let referenced = posted.referenced_message.as_deref();
    let same = |channel: Option<&str>| channel.is_none_or(|id| id.is_empty() || id == posted.channel_id);
    if !same(reference.and_then(|item| item.channel_id.as_deref())) || !same(referenced.map(|item| item.channel_id.as_str())) {
        return None;
    }
    reference.and_then(|item| item.message_id.clone()).or_else(|| (kind == 21).then(|| referenced.map(|item| item.id.clone())).flatten())
}
/// Where a crosspost or a forward was first said.
fn referenced(origin: &model::Reference, place: &Place) -> Forwarded {
    let channel = origin.channel_id.as_deref().unwrap_or("");
    let elsewhere = origin.guild_id.as_deref().filter(|guild| Some(*guild) != place.guild);
    Forwarded {
        from_title: place.names.channel(channel).map_or_else(String::new, |name| format!("#{name}")),
        from_space: elsewhere.and_then(|guild| place.names.space(guild)).unwrap_or_default(),
        link: match &origin.message_id {
            Some(message) if !channel.is_empty() => jump_link(origin.guild_id.as_deref(), channel, message),
            _ => String::new(),
        },
        ..Forwarded::default()
    }
}

/// The person and command behind an application-command reply. Component interactions have no command name here.
fn command_via(posted: &Posted) -> Option<Via> {
    let meta = posted.interaction_metadata.as_ref().or(posted.interaction.as_ref())?;
    if meta.kind != 0 && meta.kind != 2 {
        return None;
    }
    let command = meta.name.as_deref().filter(|name| !name.is_empty())?;
    let who = meta.user.as_ref().map(|user| user.name().to_owned()).filter(|name| !name.is_empty())?;
    Some(Via { who, command: command.trim_start_matches('/').to_owned() })
}

/// Flags a partial update left out, rebuilt from what the row already stored.
fn remembered_flags(message: &Message) -> u64 {
    let mut flags = message
        .controls
        .iter()
        .find_map(|control| {
            control.block_id.starts_with("discord:").then(|| super::interactions::parse_block_meta(&control.block_id).1)
        })
        .unwrap_or(0);
    if message.ephemeral {
        flags |= model::FLAG_EPHEMERAL;
    }
    if message.loading {
        flags |= model::FLAG_LOADING;
    }
    if message.silent {
        flags |= model::FLAG_SUPPRESS_NOTIFICATIONS;
    }
    flags
}

/// Every embed a bot or webhook composed (`by_bot`, type `rich`) is a card, in Discord's
/// order; every other one is a link's preview: a person's link that Discord unfurled is `rich`
/// too. A composed embed's picture rides along as a preview with nothing but the picture, and
/// an embed that repeats an earlier one's address is only another picture of it (a gallery).
fn cards(
    rich: &mut Rich,
    message: &str,
    embeds: &[Embed],
    names: &dyn Names,
    by_bot: bool,
    blur_explicit: bool,
) -> (Vec<Card>, Vec<Preview>, Vec<Attachment>) {
    let mut composed = Vec::new();
    let mut previews = Vec::new();
    let mut moving_pictures = Vec::new();
    let mut carded: Vec<&str> = Vec::new();
    for (index, embed) in embeds.iter().enumerate() {
        if let Some(attachment) = moving(rich, message, index, embed, blur_explicit) {
            moving_pictures.push(attachment);
            continue;
        }
        if !by_bot || embed.kind.as_deref() != Some("rich") {
            previews.push(preview(rich, message, index, embed, blur_explicit));
            continue;
        }
        let address = embed.url.as_deref().filter(|url| !url.is_empty());
        if !address.is_some_and(|url| carded.contains(&url)) {
            composed.push(card(rich, message, index, embed, names, blur_explicit));
            carded.extend(address);
        }
        if embed.image.is_some() {
            let picture = Embed {
                title: None,
                description: None,
                fields: Vec::new(),
                provider: None,
                author: None,
                thumbnail: None,
                ..embed.clone()
            };
            // Only when Discord's own proxy serves it: a picture elsewhere is never fetched.
            let picture = preview(rich, message, index, &picture, blur_explicit);
            if picture.image.is_some() {
                previews.push(picture);
            }
        }
    }
    (composed, previews, moving_pictures)
}

/// Applies the fields a partial `MESSAGE_UPDATE` carries to the message as stored.
pub fn patch(stored: &mut Message, update: &Posted, place: &Place) -> Files {
    let mut rich = Rich::default();
    if let Some(raw) = &update.poll {
        let (poll, files) = super::polls::convert(raw, stored.poll.as_deref());
        if let Some(poll) = poll {
            stored.poll = Some(Box::new(poll));
            stored.kind = MessageKind::Normal;
            if update.content.is_none() && stored.text.starts_with("Poll: ") {
                stored.text.clear();
            }
            rich.files.files.extend(files);
        }
    }
    if let Some(content) = &update.content {
        if stored.kind == MessageKind::Normal {
            markdown_in(&mut rich, content, place.names, place.guild);
            stored.text = std::mem::take(&mut rich.text);
            stored.spans = std::mem::take(&mut rich.spans);
            stored.invites = extract_invites(&stored.spans);
        }
    }
    if let Some(attached) = &update.attachments {
        // Stickers and embeds' moving pictures are not attachments to Discord.
        let stickers: Vec<Attachment> = stored
            .attachments
            .iter()
            .filter(|kept| kept.file.id.starts_with("sticker:") || kept.file.id.starts_with("embed:"))
            .cloned()
            .collect();
        stored.attachments = attached.iter().map(|attached| attachment(&mut rich, attached, place.blur_explicit)).collect();
        stored.attachments.extend(stickers);
    }
    if let Some(embeds) = &update.embeds {
        let (mut composed, previews, moving_pictures) = cards(&mut rich, &stored.id, embeds, place.names, stored.sender.bot, place.blur_explicit);
        stored.card = (!composed.is_empty()).then(|| Box::new(composed.remove(0)));
        stored.cards = composed;
        stored.previews = previews;
        stored.attachments.retain(|kept| !kept.file.id.starts_with("embed:"));
        stored.attachments.extend(moving_pictures);
    }
    if let Some(reacted) = &update.reactions {
        stored.reactions = reactions(&mut rich, reacted);
    }
    if let Some(components) = &update.components {
        let app_id = update
            .application_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&stored.sender.id);
        let link = if stored.permalink.is_empty() {
            jump_link(place.guild, &update.channel_id, &stored.id)
        } else {
            stored.permalink.clone()
        };
        let mut comp_rich = Rich::default();
        let mut comp_attachments = Vec::new();
        let (controls, _) = extract_components(
            components,
            &stored.id,
            app_id,
            update.flags.unwrap_or_else(|| remembered_flags(stored)),
            &link,
            place.names,
            &mut comp_rich,
            &mut comp_attachments,
        );
        stored.controls = controls;
        if !comp_rich.text.is_empty() && update.content.as_deref().unwrap_or("").is_empty() {
            stored.text = comp_rich.text;
            stored.spans = comp_rich.spans;
            if matches!(stored.kind, MessageKind::Unsupported { .. }) {
                stored.kind = MessageKind::Normal;
            }
        } else if !stored.controls.is_empty() && matches!(stored.kind, MessageKind::Unsupported { .. }) {
            stored.kind = MessageKind::Normal;
        }
        rich.files.merge(comp_rich.files);
        if !comp_attachments.is_empty() {
            stored.attachments.retain(|kept| !kept.file.id.starts_with("comp:"));
            stored.attachments.extend(comp_attachments);
        }
    }
    if let Some(pinned) = update.pinned {
        stored.pinned = pinned;
    }
    if let Some(flags) = update.flags {
        stored.published = flags & model::CROSSPOSTED != 0;
        stored.ephemeral = flags & model::FLAG_EPHEMERAL != 0;
        stored.silent = flags & model::FLAG_SUPPRESS_NOTIFICATIONS != 0;
        stored.loading = flags & model::FLAG_LOADING != 0;
        if stored.loading && stored.text.is_empty() && stored.attachments.is_empty() {
            stored.text = format!("{} is thinking…", stored.sender.name);
            if matches!(stored.kind, MessageKind::Unsupported { .. }) {
                stored.kind = MessageKind::Normal;
            }
        }
    }
    if update.interaction_metadata.is_some() || update.interaction.is_some() {
        stored.via = command_via(update);
    }
    if update.edited_timestamp.is_some() {
        stored.edited = true;
        stored.edited_at = update.edited_timestamp.as_deref().and_then(model::iso_seconds).or(stored.edited_at);
    }
    if let Some(payload) = &update.call {
        let (note, text) = build_call_note(
            Some(payload),
            &stored.sender.id,
            &stored.sender.name,
            stored.sent,
            stored.outgoing,
            place.me,
        );
        if let Some(text) = text {
            stored.text = text;
        }
        stored.call = Some(note);
    }
    rich.files
}

/// Returns `true` only for safe external HTTP(S) URLs.
fn is_safe_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Formats a component's emoji as a short display string (Unicode glyph or `:name:`).
fn component_emoji(emoji: Option<&model::Emoji>) -> String {
    let Some(emoji) = emoji else {
        return String::new();
    };
    match (&emoji.id, &emoji.name) {
        (None, Some(name)) => name.clone(),
        (Some(_), Some(name)) => format!(":{name}:"),
        _ => String::new(),
    }
}

/// Converts a Discord v2 unfurled media item into an attachment when it has a safe HTTP(S) URL.
fn component_media_attachment(
    rich: &mut Rich,
    message_id: &str,
    index: usize,
    media: &UnfurledMedia,
    as_file: bool,
    spoiler: bool,
    caption: Option<&str>,
) -> Option<Attachment> {
    let fetch_url = media
        .proxy_url
        .as_deref()
        .filter(|u| is_safe_http_url(u))
        .or_else(|| is_safe_http_url(&media.url).then_some(media.url.as_str()))?;
    let mime = media.content_type.clone().unwrap_or_else(|| {
        if as_file {
            "application/octet-stream".to_owned()
        } else {
            "image/png".to_owned()
        }
    });
    let kind = if as_file {
        MediaKind::File
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else if mime == "image/gif" {
        MediaKind::Gif
    } else {
        MediaKind::Image
    };
    let name = caption
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            fetch_url
                .split('?')
                .next()
                .and_then(|s| s.rsplit('/').next())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("media-{index}"));
    let file = MediaFile {
        width: media.width,
        height: media.height,
        ..MediaFile::new(format!("comp:{message_id}:{index}"), kind)
    };
    rich.files.push((file.clone(), fetch_url.to_owned()));
    Some(Attachment {
        alt: String::new(),
        name,
        file,
        thumb: None,
        preview: None,
        mime,
        duration: 0,
        spoiler,
        sticker: false,
        waveform: Vec::new(),
        round: false,
        view_once: false,
        sticker_set: String::new(),
        page: String::new(),
        transcript: None,
        transcription: None,
    })
}

/// Parses Discord message components (both v1 interactive action rows and v2 layout/display
/// components) into `MessageControl`s, v2 text/attachments, and whether any component was dropped.
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_components(
    components: &[Component],
    message_id: &str,
    app_id: &str,
    flags: u64,
    link: &str,
    names: &dyn Names,
    rich: &mut Rich,
    attachments: &mut Vec<Attachment>,
) -> (Vec<MessageControl>, bool) {
    let block_id = format!("discord:{app_id}:{flags}");
    let mut controls: Vec<MessageControl> = Vec::new();
    let mut dropped = false;
    let mut row_idx: u8 = 0;
    let mut media_idx: usize = 0;

    #[allow(clippy::too_many_arguments)]
    fn visit(
        comp: &Component,
        row: u8,
        row_idx: &mut u8,
        media_idx: &mut usize,
        message_id: &str,
        block_id: &str,
        names: &dyn Names,
        rich: &mut Rich,
        attachments: &mut Vec<Attachment>,
        controls: &mut Vec<MessageControl>,
        dropped: &mut bool,
    ) {
        if controls.len() >= MESSAGE_CONTROLS_LIMIT {
            *dropped = true;
            return;
        }
        match comp.kind {
            // Type 1: Action Row
            1 => {
                let this_row = *row_idx;
                *row_idx = row_idx.saturating_add(1);
                for child in &comp.components {
                    visit(child, this_row, row_idx, media_idx, message_id, block_id, names, rich, attachments, controls, dropped);
                }
            }
            // Type 2: Button
            2 => {
                let style_num = comp.style.unwrap_or(2);
                let label = comp.label.as_deref().unwrap_or("").trim().to_owned();
                let emoji = component_emoji(comp.emoji.as_ref());
                if style_num == 5 {
                    // Link button: validate URL strictly (http/https only).
                    if let Some(url) = comp.url.as_deref().filter(|u| is_safe_http_url(u)) {
                        controls.push(MessageControl {
                            block_id: block_id.to_owned(),
                            action_id: format!("link_{}", controls.len()),
                            element_type: "discord_button".to_owned(),
                            label: if label.is_empty() && emoji.is_empty() { "Open link".to_owned() } else { label },
                            value: String::new(),
                            confirm: None,
                            kind: ControlKind::Button { url: url.to_owned(), style: ControlStyle::Default },
                            disabled: comp.disabled,
                            emoji,
                            row,
                            min_values: 0,
                            max_values: 0,
                        });
                    } else {
                        *dropped = true;
                    }
                } else if style_num == 6 {
                    // Premium / SKU purchase button: not supported.
                    *dropped = true;
                } else if let Some(custom_id) = comp.custom_id.as_deref().filter(|id| !id.is_empty()) {
                    let style = match style_num {
                        1 => ControlStyle::Primary,
                        3 => ControlStyle::Success,
                        4 => ControlStyle::Danger,
                        _ => ControlStyle::Default,
                    };
                    controls.push(MessageControl {
                        block_id: block_id.to_owned(),
                        action_id: custom_id.to_owned(),
                        element_type: "discord_button".to_owned(),
                        label: if label.is_empty() && emoji.is_empty() { "Action".to_owned() } else { label },
                        value: String::new(),
                        confirm: None,
                        kind: ControlKind::Button { url: String::new(), style },
                        disabled: comp.disabled,
                        emoji,
                        row,
                        min_values: 0,
                        max_values: 0,
                    });
                } else {
                    *dropped = true;
                }
            }
            // Type 3: String Select
            3 => {
                if let Some(custom_id) = comp.custom_id.as_deref().filter(|id| !id.is_empty()) {
                    let placeholder = comp
                        .placeholder
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("Make a selection")
                        .trim()
                        .to_owned();
                    let mut selected = None;
                    let options: Vec<ControlOption> = comp
                        .options
                        .iter()
                        .map(|opt| {
                            if opt.default && selected.is_none() {
                                selected = Some(opt.value.clone());
                            }
                            let prefix = component_emoji(opt.emoji.as_ref());
                            let label = if prefix.is_empty() {
                                opt.label.clone()
                            } else {
                                format!("{prefix} {}", opt.label)
                            };
                            ControlOption {
                                label,
                                value: opt.value.clone(),
                                description: opt.description.clone().unwrap_or_default(),
                            }
                        })
                        .collect();
                    controls.push(MessageControl {
                        block_id: block_id.to_owned(),
                        action_id: custom_id.to_owned(),
                        element_type: "discord_string_select".to_owned(),
                        label: String::new(),
                        value: selected.clone().unwrap_or_default(),
                        confirm: None,
                        kind: ControlKind::Select {
                            placeholder,
                            options,
                            selected,
                            external: false,
                        },
                        disabled: comp.disabled,
                        emoji: String::new(),
                        row,
                        min_values: comp.min_values.unwrap_or(1),
                        max_values: comp.max_values.unwrap_or(1),
                    });
                } else {
                    *dropped = true;
                }
            }
            // Types 5..=8: User, Role, Mentionable, Channel selects. The shell posts the picked
            // ids the same way a string select posts its value. They are not permalinks.
            5 | 6 | 7 | 8 => {
                let (element_type, fallback_placeholder) = match comp.kind {
                    5 => ("discord_user_select", "Select a user"),
                    6 => ("discord_role_select", "Select a role"),
                    7 => ("discord_mentionable_select", "Select a user or role"),
                    _ => ("discord_channel_select", "Select a channel"),
                };
                if let Some(custom_id) = comp.custom_id.as_deref().filter(|id| !id.is_empty()) {
                    let placeholder = comp
                        .placeholder
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(fallback_placeholder)
                        .trim()
                        .to_owned();
                    let options = names
                        .choices(comp.kind)
                        .into_iter()
                        .map(|(value, label)| ControlOption {
                            label,
                            value,
                            description: String::new(),
                        })
                        .collect();
                    controls.push(MessageControl {
                        block_id: block_id.to_owned(),
                        action_id: custom_id.to_owned(),
                        element_type: element_type.to_owned(),
                        label: String::new(),
                        value: String::new(),
                        confirm: None,
                        kind: ControlKind::Select {
                            placeholder,
                            options,
                            selected: None,
                            external: false,
                        },
                        disabled: comp.disabled,
                        emoji: String::new(),
                        row,
                        min_values: comp.min_values.unwrap_or(1),
                        max_values: comp.max_values.unwrap_or(1),
                    });
                } else {
                    // No custom id, so the menu cannot be sent. The shell offers Open in Discord.
                    *dropped = true;
                }
            }
            // Type 9: Section (v2) — child text displays + optional accessory (button or thumbnail)
            9 => {
                for child in &comp.components {
                    visit(child, row, row_idx, media_idx, message_id, block_id, names, rich, attachments, controls, dropped);
                }
                if let Some(accessory) = comp.accessory.as_deref() {
                    let this_row = *row_idx;
                    if accessory.kind == 2 {
                        *row_idx = row_idx.saturating_add(1);
                    }
                    visit(accessory, this_row, row_idx, media_idx, message_id, block_id, names, rich, attachments, controls, dropped);
                }
            }
            // Type 10: Text Display (v2)
            10 => {
                if let Some(text) = comp.content.as_deref().filter(|s| !s.is_empty()) {
                    if !rich.text.is_empty() && !rich.text.ends_with('\n') {
                        rich.text.push('\n');
                    }
                    markdown(rich, text, names);
                }
            }
            // Type 11: Thumbnail (v2)
            11 => {
                if let Some(media) = comp.media.as_ref() {
                    let idx = *media_idx;
                    *media_idx += 1;
                    if let Some(att) = component_media_attachment(rich, message_id, idx, media, false, false, None) {
                        attachments.push(att);
                    }
                }
            }
            // Type 12: Media Gallery (v2)
            12 => {
                for item in &comp.items {
                    if let Some(media) = item.media.as_ref() {
                        let idx = *media_idx;
                        *media_idx += 1;
                        if let Some(att) = component_media_attachment(
                            rich,
                            message_id,
                            idx,
                            media,
                            false,
                            item.spoiler,
                            item.description.as_deref(),
                        ) {
                            attachments.push(att);
                        }
                    }
                }
            }
            // Type 13: File (v2)
            13 => {
                if let Some(media) = comp.file.as_ref() {
                    let idx = *media_idx;
                    *media_idx += 1;
                    if let Some(att) = component_media_attachment(rich, message_id, idx, media, true, false, None) {
                        attachments.push(att);
                    }
                }
            }
            // Type 14: Separator (v2)
            14 => {
                if !rich.text.is_empty() && !rich.text.ends_with("\n\n") {
                    rich.text.push('\n');
                }
            }
            // Type 17: Container (v2)
            17 => {
                for child in &comp.components {
                    visit(child, row, row_idx, media_idx, message_id, block_id, names, rich, attachments, controls, dropped);
                }
            }
            _ => {
                *dropped = true;
            }
        }
    }

    for comp in components {
        visit(
            comp,
            row_idx,
            &mut row_idx,
            &mut media_idx,
            message_id,
            &block_id,
            names,
            rich,
            attachments,
            &mut controls,
            &mut dropped,
        );
    }

    // Always offer "Open in Discord" when any component could not be rendered locally.
    if dropped && is_safe_http_url(link) && controls.len() < MESSAGE_CONTROLS_LIMIT {
        controls.push(MessageControl {
            block_id,
            action_id: "open_in_discord".to_owned(),
            element_type: "discord_open_link".to_owned(),
            label: "Open in Discord".to_owned(),
            value: String::new(),
            confirm: None,
            kind: ControlKind::Button { url: link.to_owned(), style: ControlStyle::Default },
            disabled: false,
            emoji: String::new(),
            row: row_idx,
            min_values: 0,
            max_values: 0,
        });
    }

    (controls, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nobody;

    impl Names for Nobody {
        fn user(&self, id: &str) -> Option<String> {
            (id == "7").then(|| "Priya".to_owned())
        }
        fn role(&self, _: &str) -> Option<String> {
            Some("mods".to_owned())
        }
        fn channel(&self, _: &str) -> Option<String> {
            Some("deploys".to_owned())
        }
        fn space(&self, _: &str) -> Option<String> {
            Some("Moss".to_owned())
        }
    }

    fn parsed(text: &str) -> Rich {
        let mut rich = Rich::default();
        markdown(&mut rich, text, &Nobody);
        rich
    }

    fn styled<'a>(rich: &'a Rich, style: &Style) -> Vec<&'a str> {
        let matching = rich.spans.iter().filter(|span| span.style == *style);
        matching.map(|span| &rich.text[span.start as usize..span.end as usize]).collect()
    }

    #[test]
    fn friend_request_acceptance_is_a_system_message() {
        let account = AccountKey { network: protocol::Network::Discord, account: "test".into() };
        let posted: Posted = serde_json::from_value(serde_json::json!({
            "id": "100", "channel_id": "200", "type": 67, "content": "",
            "author": {"id": "300", "username": "casimir", "global_name": "Casimir Thorne"}
        })).unwrap();
        let mut place = Place { account: &account, me: "400", guild: None, names: &Nobody, author: None, blur_explicit: true };
        let (decoded, _) = message(&posted, &place);
        assert_eq!(decoded.kind, MessageKind::System);
        assert_eq!(decoded.text, "Casimir Thorne accepted your friend request.");
        place.author = Some("Cas".into());
        let (decoded, _) = message(&posted, &place);
        assert_eq!(decoded.text, "Cas accepted your friend request.");
        assert!(decoded.spans.is_empty());

        let unknown = Posted { kind: Some(255), ..posted };
        assert!(matches!(message(&unknown, &place).0.kind, MessageKind::Unsupported { .. }));
    }

    #[test]
    fn inline_styles_leave_no_markup_and_cover_their_words() {
        let rich = parsed("a **bold _both_** ~~gone~~ __under__ ||secret|| `x*y` snake_case_name 2 * 3 * 4");
        assert_eq!(rich.text, "a bold both gone under secret x*y snake_case_name 2 * 3 * 4");
        assert_eq!(styled(&rich, &Style::Bold), ["bold both"]);
        assert_eq!(styled(&rich, &Style::Italic), ["both"]);
        assert_eq!(styled(&rich, &Style::Strike), ["gone"]);
        assert_eq!(styled(&rich, &Style::Underline), ["under"]);
        assert_eq!(styled(&rich, &Style::Spoiler), ["secret"]);
        assert_eq!(styled(&rich, &Style::Code), ["x*y"]);
    }

    #[test]
    fn mentions_read_as_names_and_emoji_as_their_name() {
        let rich = parsed("<@7> <@!9> in <#5> cc <@&3> @everyone <:party:42> <a:spin:43> </ship:123>");
        assert_eq!(rich.text, "@Priya @someone in #deploys cc @mods @everyone :party: :spin: /ship");
        let user = Style::Mention { kind: MentionKind::User, id: "7".into() };
        assert_eq!(styled(&rich, &user), ["@Priya"]);
        let everyone = Style::Mention { kind: MentionKind::Everyone, id: "everyone".into() };
        assert_eq!(styled(&rich, &everyone), ["@everyone"]);
        let command = Style::Mention { kind: MentionKind::Command, id: "123".into() };
        assert_eq!(styled(&rich, &command), ["/ship"]);
        let emoji: Vec<&str> = rich.files.files.iter().map(|(file, _)| file.id.as_str()).collect();
        assert_eq!(emoji, ["emoji:42", "emoji:43"]);
    }

    #[test]
    fn links_bare_masked_and_bracketed() {
        let rich = parsed("see https://a.example/x?y=1. and [the docs](https://b.example) or <https://c.example>");
        assert_eq!(rich.text, "see https://a.example/x?y=1. and the docs or https://c.example");
        let link = |url: &str| Style::Link { url: url.into() };
        assert_eq!(styled(&rich, &link("https://a.example/x?y=1")), ["https://a.example/x?y=1"]);
        assert_eq!(styled(&rich, &link("https://b.example")), ["the docs"]);
        assert_eq!(styled(&rich, &link("https://c.example")), ["https://c.example"]);
    }

    #[test]
    fn blocks_code_quotes_and_lists() {
        let rich = parsed("# Big\n## Mid *em*\n### Small\n-# footnote **note**\nbefore\n```rust\nlet a = *b;\n```\n> quoted **line**\n- one\n1. two\nénd");
        assert_eq!(rich.text, "Big\nMid em\nSmall\nfootnote note\nbefore\nlet a = *b;\nquoted line\none\n1. two\nénd");
        assert_eq!(styled(&rich, &Style::Heading { level: 1 }), ["Big"]);
        assert_eq!(styled(&rich, &Style::Heading { level: 2 }), ["Mid em"]);
        assert_eq!(styled(&rich, &Style::Italic), ["em"]);
        assert_eq!(styled(&rich, &Style::Heading { level: 3 }), ["Small"]);
        assert_eq!(styled(&rich, &Style::Small), ["footnote note"]);
        assert_eq!(styled(&rich, &Style::Bold), ["note", "line"]);
        assert_eq!(styled(&rich, &Style::CodeBlock { language: "rust".into() }), ["let a = *b;"]);
        assert_eq!(styled(&rich, &Style::Quote), ["quoted line"]);
        assert_eq!(styled(&rich, &Style::ListItem { ordered: false, depth: 0 }), ["one"]);
        assert_eq!(styled(&rich, &Style::ListItem { ordered: true, depth: 0 }), ["1. two"]);
        for span in &rich.spans {
            assert!(rich.text.is_char_boundary(span.start as usize) && rich.text.is_char_boundary(span.end as usize));
        }
    }

    #[test]
    fn a_picture_is_registered_at_the_size_it_is_drawn_and_at_larger_ones_up_to_its_own() {
        // Wide, tall, and already small enough.
        assert_eq!(fitted(1600, 900), (400, 225));
        assert_eq!(fitted(900, 1600), (168, 300));
        assert_eq!(fitted(320, 200), (320, 200));
        let sides = |sizes: Vec<(u32, String)>| sizes.into_iter().map(|(side, _)| side).collect::<Vec<_>>();
        let proxy = "https://media.discordapp.net/attachments/1/2/a.png?ex=1";
        // A 2x screen showing the 400 pt picture asks for 800: there is a size that large.
        assert_eq!(sides(larger(proxy, (400, 225), (1600, 900), false)), [600, 800, 1200]);
        // Never a size the picture does not have: its own is the last one.
        let small = larger(proxy, (400, 225), (700, 394), false);
        assert_eq!(sides(small.clone()), [600, 700]);
        assert!(small[1].1.ends_with("&format=webp&width=700&height=394"), "{}", small[1].1);
        assert_eq!(sides(larger(proxy, (320, 200), (320, 200), false)), Vec::<u32>::new());
        // A GIF is asked for moving at every size, never as its whole original.
        let gif = larger("https://media.discordapp.net/attachments/1/2/hop.gif?ex=1", (400, 225), (800, 450), true);
        assert!(gif.iter().all(|(_, url)| url.ends_with("&animated=true") && url.contains("format=webp")), "{gif:?}");
        assert!(may_move("https://images-ext-1.discordapp.net/external/x/https/example.org/a.GIF?width=1") && !may_move("https://x/a.png"));
    }

    #[test]
    fn unclosed_markup_stays_as_typed() {
        assert_eq!(parsed("2 ** 3 and a_b and `open").text, "2 ** 3 and a_b and `open");
        assert_eq!(parsed("\\*not italic\\*").text, "*not italic*");
    }

    #[test]
    fn timestamp_tokens_emit_time_spans_over_neutral_fallback_text() {
        let rich = parsed("Starts <t:1618953630:R> (default <t:1618953630>, short <t:1618953630:t>, long <t:1618953630:F>)");
        let times: Vec<(i64, char, &str)> = rich
            .spans
            .iter()
            .filter_map(|span| match span.style {
                Style::Time { unix, form } => Some((unix, form, &rich.text[span.start as usize..span.end as usize])),
                _ => None,
            })
            .collect();
        assert_eq!(times.len(), 4);
        assert_eq!((times[0].0, times[0].1), (1_618_953_630, 'R'));
        assert_eq!((times[1].0, times[1].1), (1_618_953_630, 'f'));
        assert_eq!((times[2].0, times[2].1), (1_618_953_630, 't'));
        assert_eq!((times[3].0, times[3].1), (1_618_953_630, 'F'));
        // Fallback text is non-empty and neutral (never stale "ago"/"in " words for relative).
        for (_, _, fallback) in &times {
            assert!(!fallback.is_empty());
        }
        assert!(!times[0].2.contains("ago") && !times[0].2.starts_with("in "));

        // Invalid timestamps stay literal.
        let bad = parsed("<t:notanumber:R>");
        assert_eq!(bad.text, "<t:notanumber:R>");
        assert!(bad.spans.is_empty());
    }

    #[test]
    fn invite_and_event_links_are_detected_and_deduplicated() {
        let rich = parsed(
            "Join https://discord.gg/rustlang?event=9001 or https://discord.com/invite/rustlang?event=9001 \
             and event https://discord.com/events/4000/8002 (ignore https://example.com/invite/fake)",
        );
        let invites = extract_invites(&rich.spans);
        assert_eq!(invites.len(), 2);
        assert_eq!(invites[0].code, "rustlang");
        assert_eq!(invites[0].event.as_ref().map(|e| e.id.as_str()), Some("9001"));
        assert_eq!(invites[0].url, "https://discord.gg/rustlang?event=9001");
        assert!(invites[1].code.is_empty());
        assert_eq!(invites[1].space_id.as_deref(), Some("4000"));
        assert_eq!(invites[1].event.as_ref().map(|e| e.id.as_str()), Some("8002"));
        assert_eq!(invites[1].url, "https://discord.com/events/4000/8002");
    }

    #[test]
    fn discord_call_message_maps_missed_ended_and_live_outcomes() {
        struct Empty;
        impl Names for Empty {
            fn user(&self, _: &str) -> Option<String> { None }
            fn role(&self, _: &str) -> Option<String> { None }
            fn channel(&self, _: &str) -> Option<String> { None }
            fn space(&self, _: &str) -> Option<String> { None }
        }
        let account = AccountKey { network: protocol::Network::Discord, account: "acct".into() };
        let place = Place { account: &account, me: "me", guild: None, names: &Empty, author: None, blur_explicit: true };
        // Snowflake 1000000000000000000 -> (1000000000000000000 >> 22) + 1420070400000 = 1658489056s (2022-07-22T11:24:16Z)
        let base = Posted {
            id: "1000000000000000000".into(),
            channel_id: "dm-1".into(),
            author: Some(model::User { id: "peer".into(), username: "bea".into(), global_name: Some("Bea".into()), ..Default::default() }),
            kind: Some(3),
            ..Default::default()
        };
        let mut missed = base.clone();
        missed.call = Some(model::CallPayload { participants: vec!["peer".into()], ended_timestamp: Some("2022-07-22T11:24:46Z".into()) });
        let (m, _) = message(&missed, &place);
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Missed));
        assert_eq!(m.call.as_ref().map(|c| c.participants.as_slice()), Some(&["peer".to_owned()][..]));
        assert_eq!(m.text, "Missed call from Bea");

        // Group call where two others joined, but I did not: still missed for me.
        let mut group_missed = base.clone();
        group_missed.call = Some(model::CallPayload {
            participants: vec!["peer".into(), "other".into()],
            ended_timestamp: Some("2022-07-22T11:30:00Z".into()),
        });
        let (m, _) = message(&group_missed, &place);
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Missed));
        assert_eq!(m.call.as_ref().map(|c| c.participants.len()), Some(2));

        let mut ended = base.clone();
        ended.call = Some(model::CallPayload { participants: vec!["peer".into(), "me".into()], ended_timestamp: Some("2022-07-22T11:34:59Z".into()) });
        let (m, _) = message(&ended, &place);
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Ended));
        assert_eq!(m.call.as_ref().and_then(|c| c.seconds), Some(720));
        assert_eq!(m.call.as_ref().map(|c| c.participants.len()), Some(2));
        assert_eq!(m.text, "Call with Bea · 12 min");

        let mut live = base;
        live.call = Some(model::CallPayload { participants: vec!["peer".into()], ended_timestamp: None });
        let (mut m, _) = message(&live, &place);
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Live));

        // Partial MESSAGE_UPDATE when the call ends updates outcome, duration, participants and text.
        let update = Posted {
            id: "1000000000000000000".into(),
            channel_id: "dm-1".into(),
            call: Some(model::CallPayload {
                participants: vec!["peer".into(), "me".into()],
                ended_timestamp: Some("2022-07-22T11:34:59Z".into()),
            }),
            ..Default::default()
        };
        patch(&mut m, &update, &place);
        assert_eq!(m.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Ended));
        assert_eq!(m.call.as_ref().and_then(|c| c.seconds), Some(720));
        assert_eq!(m.call.as_ref().map(|c| c.participants.len()), Some(2));
        assert_eq!(m.text, "Call with Bea · 12 min");
    }

    #[test]
    fn entity_selects_offer_ids_instead_of_a_permalink() {
        struct Listed;
        impl Names for Listed {
            fn user(&self, _: &str) -> Option<String> {
                None
            }
            fn role(&self, _: &str) -> Option<String> {
                None
            }
            fn channel(&self, _: &str) -> Option<String> {
                None
            }
            fn space(&self, _: &str) -> Option<String> {
                None
            }
            fn choices(&self, kind: u8) -> Vec<(String, String)> {
                match kind {
                    5 => vec![("2001".into(), "Mara".into())],
                    6 => vec![("4000".into(), "mods".into())],
                    8 => vec![("4101".into(), "general".into())],
                    _ => Vec::new(),
                }
            }
        }
        let comps: Vec<Component> = serde_json::from_value(serde_json::json!([
            {"type": 5, "custom_id": "who", "placeholder": "Who"},
            {"type": 6, "custom_id": "which_role"},
            {"type": 8, "custom_id": "which_channel"}
        ]))
        .unwrap();
        let mut rich = Rich::default();
        let mut attachments = Vec::new();
        let (controls, dropped) = extract_components(&comps, "m1", "app", 0, "https://discord.com/channels/g/c/m1", &Listed, &mut rich, &mut attachments);
        assert!(!dropped);
        assert_eq!(controls.len(), 3);
        assert!(matches!(
            &controls[0].kind,
            ControlKind::Select { external: false, options, .. }
                if options.iter().any(|opt| opt.value == "2001" && opt.label == "Mara")
        ));
        assert_eq!(controls[0].element_type, "discord_user_select");
        assert!(matches!(
            &controls[1].kind,
            ControlKind::Select { external: false, options, .. } if options.iter().any(|opt| opt.value == "4000")
        ));
        assert!(matches!(
            &controls[2].kind,
            ControlKind::Select { external: false, options, .. } if options.iter().any(|opt| opt.value == "4101" && opt.label == "general")
        ));
        assert_eq!(controls[2].element_type, "discord_channel_select");
    }

    #[test]
    fn components_v1_and_v2_extract_controls_text_and_open_in_discord_fallback() {
        struct Empty;
        impl Names for Empty {
            fn user(&self, _: &str) -> Option<String> { None }
            fn role(&self, _: &str) -> Option<String> { None }
            fn channel(&self, _: &str) -> Option<String> { None }
            fn space(&self, _: &str) -> Option<String> { None }
        }
        let account = AccountKey { network: protocol::Network::Discord, account: "acct".into() };
        let place = Place { account: &account, me: "me", guild: Some("4000"), names: &Empty, author: None, blur_explicit: false };

        // 1. V1 action rows: primary/success/danger/link buttons (invalid link rejected) + string select.
        let posted: Posted = serde_json::from_value(serde_json::json!({
            "id": "1300000000000000100",
            "channel_id": "4101",
            "application_id": "9001",
            "author": {"id": "9001", "username": "DeployBot", "bot": true},
            "content": "Choose a target:",
            "flags": 0,
            "components": [
                {
                    "type": 1,
                    "components": [
                        {"type": 2, "style": 3, "label": "Approve", "custom_id": "approve_1", "emoji": {"name": "✅"}},
                        {"type": 2, "style": 4, "label": "Reject", "custom_id": "reject_1", "disabled": true},
                        {"type": 2, "style": 5, "label": "Docs", "url": "https://example.com/docs"},
                        {"type": 2, "style": 5, "label": "BadLink", "url": "javascript:alert(1)"}
                    ]
                },
                {
                    "type": 1,
                    "components": [
                        {
                            "type": 3,
                            "custom_id": "env_pick",
                            "placeholder": "Pick env",
                            "min_values": 1,
                            "max_values": 2,
                            "options": [
                                {"label": "Staging", "value": "staging", "description": "Pre-prod", "default": true},
                                {"label": "Prod", "value": "prod"}
                            ]
                        }
                    ]
                }
            ]
        }))
        .unwrap();
        let (mut m, _) = message(&posted, &place);
        assert_eq!(m.kind, MessageKind::Normal);
        // 3 valid buttons + 1 string select + 1 "Open in Discord" fallback (because BadLink was dropped).
        assert_eq!(m.controls.len(), 5);
        assert_eq!(m.controls[0].action_id, "approve_1");
        assert_eq!(m.controls[0].emoji, "✅");
        assert_eq!(m.controls[0].row, 0);
        assert_eq!(m.controls[0].block_id, "discord:9001:0");
        assert_eq!(m.controls[0].kind, ControlKind::Button { url: String::new(), style: ControlStyle::Success });
        assert!(m.controls[1].disabled);
        assert_eq!(m.controls[2].kind, ControlKind::Button { url: "https://example.com/docs".into(), style: ControlStyle::Default });
        assert_eq!(m.controls[3].row, 1);
        assert_eq!((m.controls[3].min_values, m.controls[3].max_values), (1, 2));
        assert!(matches!(&m.controls[3].kind, ControlKind::Select { selected: Some(s), options, .. } if s == "staging" && options[0].description == "Pre-prod"));
        assert_eq!(m.controls[4].label, "Open in Discord");
        assert!(matches!(&m.controls[4].kind, ControlKind::Button { url, .. } if url == "https://discord.com/channels/4000/4101/1300000000000000100"));

        // 2. Partial MESSAGE_UPDATE replaces controls (e.g. disabling button after press).
        let upd: Posted = serde_json::from_value(serde_json::json!({
            "id": "1300000000000000100",
            "channel_id": "4101",
            "components": [
                {"type": 1, "components": [{"type": 2, "style": 3, "label": "Approved", "custom_id": "approve_1", "disabled": true}]}
            ]
        }))
        .unwrap();
        patch(&mut m, &upd, &place);
        assert_eq!(m.controls.len(), 1);
        assert_eq!(m.controls[0].label, "Approved");
        assert!(m.controls[0].disabled);

        // 3. Components V2: Container + Section + Text Display + Button accessory + Media Gallery.
        let v2: Posted = serde_json::from_value(serde_json::json!({
            "id": "1300000000000000101",
            "channel_id": "4101",
            "author": {"id": "9001", "username": "Bot", "bot": true},
            "content": "",
            "flags": 32768,
            "components": [
                {
                    "type": 17,
                    "components": [
                        {
                            "type": 9,
                            "components": [{"type": 10, "content": "**Release v2** is ready"}],
                            "accessory": {"type": 2, "style": 1, "label": "Ship", "custom_id": "ship_v2"}
                        },
                        {
                            "type": 12,
                            "items": [{"media": {"url": "https://cdn.discordapp.com/a.png", "width": 640, "height": 480, "content_type": "image/png"}}]
                        }
                    ]
                }
            ]
        }))
        .unwrap();
        let (m2, files2) = message(&v2, &place);
        assert_eq!(m2.kind, MessageKind::Normal);
        assert_eq!(m2.text, "Release v2 is ready");
        assert_eq!(m2.controls.len(), 1);
        assert_eq!(m2.controls[0].label, "Ship");
        assert_eq!(m2.attachments.len(), 1);
        assert!(files2.files.iter().any(|(_, url)| url == "https://cdn.discordapp.com/a.png"));
    }

    #[test]
    fn a_command_reply_names_who_used_it_and_marks_ephemeral_silent_and_thinking() {
        let account = AccountKey { network: protocol::Network::Discord, account: "test".into() };
        let place = Place { account: &account, me: "400", guild: Some("4000"), names: &Nobody, author: None, blur_explicit: false };
        let posted: Posted = serde_json::from_value(serde_json::json!({
            "id": "1300000000000000200",
            "channel_id": "4101",
            "type": 20,
            "content": "shipped",
            "flags": 64,
            "author": {"id": "9001", "username": "helper", "global_name": "Helper", "bot": true},
            "interaction_metadata": {
                "id": "9100",
                "type": 2,
                "name": "ship",
                "user": {"id": "7", "username": "priya", "global_name": "Priya"}
            },
            "message_reference": {"type": 0, "channel_id": "4101", "guild_id": "5000", "message_id": "1300000000000000100"}
        }))
        .unwrap();
        let (mut decoded, _) = message(&posted, &place);
        assert!(decoded.ephemeral);
        assert!(!decoded.loading && !decoded.silent);
        assert_eq!(decoded.kind, MessageKind::Normal);
        assert_eq!(decoded.text, "shipped");
        let via = decoded.via.as_ref().unwrap();
        assert_eq!((via.who.as_str(), via.command.as_str()), ("Priya", "ship"));

        let mut quiet = posted.clone();
        quiet.flags = Some(model::FLAG_SUPPRESS_NOTIFICATIONS | 2);
        quiet.interaction_metadata = None;
        quiet.content = Some("followed".into());
        let (cross, _) = message(&quiet, &place);
        assert!(cross.silent);
        let from = cross.forwarded.unwrap();
        assert_eq!((from.from_title.as_str(), from.from_space.as_str()), ("#deploys", "Moss"));

        let mut thinking = posted.clone();
        thinking.content = Some(String::new());
        thinking.flags = Some(model::FLAG_LOADING);
        thinking.interaction = Some(model::InteractionMeta {
            kind: 2,
            name: Some("ship".into()),
            user: Some(model::User { id: "7".into(), username: "priya".into(), global_name: Some("Priya".into()), ..Default::default() }),
        });
        thinking.interaction_metadata = None;
        let (thought, _) = message(&thinking, &place);
        assert!(thought.loading);
        assert_eq!(thought.text, "Helper is thinking…");
        assert_eq!(thought.kind, MessageKind::Normal);
        assert_eq!(thought.via.unwrap().command, "ship");

        // A partial update that omits flags must not clear the only-you mark.
        let update = Posted { content: Some("still here".into()), ..Posted::default() };
        assert!(update.flags.is_none());
        patch(&mut decoded, &update, &place);
        assert!(decoded.ephemeral);
        assert_eq!(decoded.text, "still here");
        assert_eq!(decoded.via.as_ref().map(|via| via.command.as_str()), Some("ship"));
    }

    #[test]
    fn message_in_guild_uses_server_avatar_while_dm_keeps_global_avatar() {
        let account = AccountKey { network: protocol::Network::Discord, account: "test".into() };
        let posted: Posted = serde_json::from_value(serde_json::json!({
            "id": "100", "channel_id": "4101", "type": 0, "content": "hi",
            "author": {"id": "2001", "username": "mara", "global_name": "Mara", "avatar": "global1"},
            "member": {"nick": "MaraGuild", "avatar": "guild1"}
        }))
        .unwrap();
        let guild_place = Place { account: &account, me: "1000", guild: Some("4000"), names: &Nobody, author: None, blur_explicit: false };
        let (in_guild, files) = message(&posted, &guild_place);
        assert_eq!(in_guild.sender.avatar.as_ref().map(|a| a.id.as_str()), Some("mavatar:4000:2001:guild1"));
        assert!(files.files.iter().any(|(f, u)| f.id == "mavatar:4000:2001:guild1" && u.contains("/guilds/4000/users/2001/avatars/guild1.webp")));

        let dm_place = Place { account: &account, me: "1000", guild: None, names: &Nobody, author: None, blur_explicit: false };
        let (in_dm, dm_files) = message(&posted, &dm_place);
        assert_eq!(in_dm.sender.avatar.as_ref().map(|a| a.id.as_str()), Some("avatar:2001:global1"));
        assert!(dm_files.files.iter().any(|(f, u)| f.id == "avatar:2001:global1" && u.contains("/avatars/2001/global1.webp")));
    }

    #[test]
    fn multi_snapshot_forward_renders_all_snapshots_and_attachments() {
        struct Known;
        impl Names for Known {
            fn user(&self, _: &str) -> Option<String> { None }
            fn role(&self, _: &str) -> Option<String> { None }
            fn channel(&self, id: &str) -> Option<String> { (id == "4101").then(|| "general".into()) }
            fn space(&self, id: &str) -> Option<String> { (id == "4000").then(|| "Rustaceans".into()) }
        }
        let account = AccountKey { network: protocol::Network::Discord, account: "acct".into() };
        let place = Place { account: &account, me: "me", guild: None, names: &Known, author: None, blur_explicit: false };
        let snap1 = Posted {
            content: Some("First snapshot".into()),
            attachments: Some(vec![Attached {
                id: "1".into(),
                filename: "a.txt".into(),
                size: 12,
                url: "https://cdn.discordapp.com/attachments/4101/1/a.txt".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let snap2 = Posted {
            content: Some("Second snapshot".into()),
            attachments: Some(vec![Attached {
                id: "2".into(),
                filename: "b.txt".into(),
                size: 24,
                url: "https://cdn.discordapp.com/attachments/4101/2/b.txt".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let posted = Posted {
            id: "1300000000000000999".into(),
            channel_id: "3001".into(),
            author: Some(model::User { id: "2001".into(), username: "mara".into(), global_name: Some("Mara".into()), ..Default::default() }),
            message_reference: Some(model::Reference {
                kind: 1,
                channel_id: Some("4101".into()),
                message_id: Some("1300000000000000100".into()),
                guild_id: Some("4000".into()),
            }),
            message_snapshots: vec![
                model::Snapshot { message: Box::new(snap1) },
                model::Snapshot { message: Box::new(snap2) },
            ],
            ..Default::default()
        };
        let (msg, _) = message(&posted, &place);
        assert!(msg.text.contains("First snapshot"), "missing first snapshot: {}", msg.text);
        assert!(msg.text.contains("Second snapshot"), "missing second snapshot: {}", msg.text);
        assert_eq!(msg.attachments.len(), 2);
        let fwd = msg.forwarded.as_ref().expect("forwarded metadata");
        assert_eq!(fwd.from_title, "#general");
        assert_eq!(fwd.from_space, "Rustaceans");
        assert_eq!(fwd.link, "https://discord.com/channels/4000/4101/1300000000000000100");
    }

    #[test]
    fn every_system_message_type_has_wording_boost_tiers_and_jump_targets() {
        let account = AccountKey { network: protocol::Network::Discord, account: "test".into() };
        let place = Place { account: &account, me: "400", guild: Some("4000"), names: &Nobody, author: None, blur_explicit: false };
        let make = |kind: u8, content: &str| -> Posted {
            serde_json::from_value(serde_json::json!({
                "id": "100", "channel_id": "200", "type": kind, "content": content,
                "author": {"id": "300", "username": "ana", "global_name": "Ana"},
                "mentions": [{"id": "301", "username": "ben", "global_name": "Ben"}]
            }))
            .unwrap()
        };

        let table: &[(u8, &str, &str)] = &[
            (1, "", "Ana added Ben to the group."),
            (2, "", "Ana removed Ben from the group."),
            (3, "", "Ana started a call."),
            (4, "Trail crew", "Ana changed the group name: Trail crew"),
            (5, "", "Ana changed the group icon."),
            (6, "", "Ana pinned a message."),
            (7, "", "Ana joined the server."),
            (8, "", "Ana boosted the server."),
            (8, "3", "Ana boosted the server 3 times!"),
            (9, "", "Ana boosted the server! Moss has reached Level 1."),
            (10, "2", "Ana boosted the server 2 times! Moss has reached Level 2."),
            (11, "4", "Ana boosted the server 4 times! Moss has reached Level 3."),
            (12, "#news", "Ana added #news to this channel."),
            (14, "", "This server has been removed from Server Discovery because it no longer passes all the requirements."),
            (15, "", "This server is eligible for Server Discovery again and has been automatically relisted!"),
            (16, "", "This server has failed Discovery activity requirements for 1 week. If this server fails for 4 weeks in a row, it will be automatically removed from Discovery."),
            (17, "", "This server has failed Discovery activity requirements for 3 weeks in a row. If this server fails for 1 more week, it will be automatically removed from Discovery."),
            (18, "Release plan", "Ana started a thread: Release plan"),
            (21, "", "Ana started a thread."),
            (22, "", "Sending invites to your server is the best way to grow your community."),
            (24, "", "AutoMod flagged a message."),
            (25, "", "Ana subscribed to Moss."),
            (26, "", "Upgrade to unlock more features for this app."),
            (27, "Town Hall", "Ana started Town Hall"),
            (28, "Town Hall", "Ana ended Town Hall"),
            (29, "", "Ana is now a speaker."),
            (30, "", "Ana requested to speak."),
            (31, "Q&A", "Ana changed the Stage topic: Q&A"),
            (32, "", "Ana subscribed to an app for Moss."),
            (36, "", "Ana enabled security actions for this server."),
            (37, "", "Ana disabled security actions for this server."),
            (38, "", "Ana reported a raid in this server."),
            (39, "", "Ana marked the raid alert as a false alarm."),
            (44, "", "Ana made a purchase."),
            (46, "", "A poll has closed."),
            (67, "", "Ana accepted your friend request."),
        ];
        for &(kind, content, expected) in table {
            let posted = make(kind, content);
            let (decoded, _) = message(&posted, &place);
            assert_eq!(decoded.kind, MessageKind::System, "type {kind} must be System");
            assert_eq!(decoded.text, expected, "type {kind} wording mismatch");
        }

        // Pin (type 6) and poll result (type 46) carry their jump target.
        let mut pin = make(6, "");
        pin.message_reference = Some(model::Reference { message_id: Some("9001".into()), channel_id: Some("200".into()), guild_id: Some("4000".into()), kind: 0 });
        let (pin_msg, _) = message(&pin, &place);
        assert_eq!(pin_msg.jump.as_deref(), Some("9001"));

        let mut poll_res = make(46, "");
        poll_res.message_reference = Some(model::Reference { message_id: Some("9002".into()), channel_id: Some("200".into()), guild_id: Some("4000".into()), kind: 0 });
        let (poll_msg, _) = message(&poll_res, &place);
        assert_eq!(poll_msg.jump.as_deref(), Some("9002"));

        // A pin in another channel is quoted only when the message is included; it is not a jump.
        let mut far_pin = make(6, "");
        far_pin.message_reference = Some(model::Reference {
            message_id: Some("9003".into()),
            channel_id: Some("999".into()),
            guild_id: Some("4000".into()),
            kind: 0,
        });
        assert!(message(&far_pin, &place).0.jump.is_none());

        // Thread starter (type 21) quotes referenced_message. A starter in another channel is not a jump.
        let starter: Posted = serde_json::from_value(serde_json::json!({
            "id": "500", "channel_id": "4500", "type": 21, "content": "",
            "author": {"id": "300", "username": "ana", "global_name": "Ana"},
            "message_reference": {"message_id": "499", "channel_id": "4101", "guild_id": "4000", "type": 0},
            "referenced_message": {
                "id": "499", "channel_id": "4101", "type": 0, "content": "Let's plan the release",
                "author": {"id": "301", "username": "ben", "global_name": "Ben"}
            }
        }))
        .unwrap();
        let (starter_msg, _) = message(&starter, &place);
        assert_eq!(starter_msg.kind, MessageKind::System);
        assert!(starter_msg.jump.is_none(), "a thread starter does not jump into its parent channel");
        let quote = starter_msg.reply_to.as_ref().expect("thread starter carries quoted original");
        assert_eq!((quote.id.as_str(), quote.sender.as_str(), quote.snippet.as_str()), ("499", "Ben", "Let's plan the release"));

        // Role subscription purchase (type 25) and purchase notification (type 44) use structured metadata.
        let mut sub = make(25, "");
        sub.role_subscription_data = Some(model::RoleSubscriptionData {
            tier_name: "Supporter".into(),
            total_months_subscribed: 6,
            is_renewal: true,
        });
        assert_eq!(message(&sub, &place).0.text, "Ana renewed Supporter and has been a subscriber of Moss for 6 months!");

        let mut purchase = make(44, "");
        purchase.purchase_notification = Some(model::PurchaseNotification {
            guild_product_purchase: Some(model::GuildProductPurchase { product_name: "Field Guide".into() }),
        });
        assert_eq!(message(&purchase, &place).0.text, "Ana has purchased Field Guide!");

        // Unknown future type still falls back to Unsupported.
        let unknown = make(250, "");
        let (unsupported, _) = message(&unknown, &place);
        assert!(matches!(unsupported.kind, MessageKind::Unsupported { .. }));
    }

    #[test]
    fn explicit_media_is_blurred_when_filter_is_on_and_shown_when_off() {
        let account = AccountKey { network: protocol::Network::Discord, account: "acct".into() };
        let posted = Posted {
            id: "1000000000000000001".into(),
            channel_id: "c1".into(),
            author: Some(model::User { id: "2001".into(), username: "bea".into(), ..Default::default() }),
            attachments: Some(vec![Attached {
                title: None, description: None,
                id: "9001".into(),
                filename: "pic.png".into(),
                size: 1024,
                url: "https://cdn.discordapp.com/attachments/1/9001/pic.png".into(),
                proxy_url: Some("https://media.discordapp.net/attachments/1/9001/pic.png".into()),
                width: 400,
                height: 300,
                content_type: Some("image/png".into()),
                duration_secs: 0.0,
                waveform: None,
                flags: 1 << 4,
            }]),
            ..Default::default()
        };
        let blur = Place { account: &account, me: "me", guild: None, names: &Nobody, author: None, blur_explicit: true };
        let (blurred, _) = message(&posted, &blur);
        assert!(blurred.attachments[0].spoiler, "explicit attachment blurs when filter is active");

        let show = Place { blur_explicit: false, ..blur };
        let (shown, _) = message(&posted, &show);
        assert!(!shown.attachments[0].spoiler, "explicit attachment unblurred when filter is set to show");
    }

    #[test]
    fn discord_channel_and_message_links_and_special_tokens_parse() {
        struct Partial;
        impl Names for Partial {
            fn user(&self, _: &str) -> Option<String> { None }
            fn role(&self, _: &str) -> Option<String> { None }
            fn channel(&self, id: &str) -> Option<String> {
                (id == "4102").then(|| "announcements".to_owned())
            }
            fn space(&self, _: &str) -> Option<String> { None }
        }
        let mut rich = Rich::default();
        markdown_in(
            &mut rich,
            "see <#4102> <#9999> https://discord.com/channels/4000/4102/1300001 <https://ptb.discord.com/channels/4000/4102> https://canary.discordapp.com/channels/@me/9999/1300002 [jump](https://discord.com/channels/4000/4102/1300003) <id:browse> <id:customize> <id:guide>",
            &Partial,
            Some("4000"),
        );
        assert_eq!(
            rich.text,
            "see #announcements # No Access #announcements › message #announcements # No Access › message jump Browse Channels Channels & Roles Server Guide"
        );
        let ch_mention = Style::Mention { kind: MentionKind::Channel, id: "4102".into() };
        assert_eq!(
            styled(&rich, &ch_mention),
            ["#announcements", "#announcements › message", "#announcements", "jump"]
        );
        let no_access = Style::Mention { kind: MentionKind::Channel, id: "9999".into() };
        assert_eq!(styled(&rich, &no_access), ["# No Access", "# No Access › message"]);
        assert_eq!(
            styled(&rich, &Style::Link { url: "https://discord.com/channels/4000/4102/1300001".into() }),
            ["#announcements › message"]
        );
        assert_eq!(
            styled(&rich, &Style::Link { url: "https://discord.com/channels/4000/channel-browser".into() }),
            ["Browse Channels"]
        );
        assert_eq!(
            styled(&rich, &Style::Link { url: "https://discord.com/channels/4000/customize-community".into() }),
            ["Channels & Roles"]
        );
        assert_eq!(
            styled(&rich, &Style::Link { url: "https://discord.com/channels/4000/home".into() }),
            ["Server Guide"]
        );
    }
}
