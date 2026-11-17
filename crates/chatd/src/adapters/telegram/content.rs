//! Telegram's messages into the one message model: entities to spans, media to attachments,
//! service messages to one grey line, and whatever tinyclient cannot draw to a named line with a
//! link into Telegram's own app. Nothing here asks the network anything.

use super::peers::{is_replies, message_link, thread_id, Directory, Who};
use grammers_tl_types as tl;
use protocol::{
    Attachment, Forwarded, MediaFile, MediaKind, MentionKind, Message, MessageKind, Preview, Reaction, Sender, Span,
    Style, ThreadSummary,
};
use tl::enums::{DocumentAttribute, MessageAction, MessageEntity, MessageMedia, PhotoSize};

/// A file a message shows, and where Telegram keeps it.
#[derive(Debug, Clone)]
pub struct Want {
    pub file: MediaFile,
    pub dc: i32,
    pub location: tl::enums::InputFileLocation,
    /// The bytes themselves, when Telegram sent them inside the message.
    pub inline: Option<Vec<u8>>,
    /// The conversation whose row shows it, for a chat's picture: the row is read again.
    pub row: Option<String>,
    /// Asked for as the picture of a row the shell lists as on screen: it waits, unfetched,
    /// while that row has scrolled away, and goes back in line when the row is on screen
    /// again. Only what `Watch` named as rows says so; a message's own pictures never wait.
    pub watched: bool,
    /// A picture itself, for the viewer: kept until the shell asks for it, never fetched for a
    /// row.
    pub original: bool,
}

pub struct Mapped {
    pub peer: Who,
    pub id: i32,
    /// The forum topic its reply header names. `None` in a forum means the General topic.
    pub topic: Option<i32>,
    /// The root of a discussion thread, distinct from the comment this message answers.
    pub discussion_top: Option<i32>,
    /// The message it answers, when it is a reply and not just a post in a topic.
    pub reply: Option<i32>,
    pub message: Message,
    /// What it shows inline (thumbnails and stills), and the originals of its pictures, which
    /// are only fetched when the viewer asks.
    pub wants: Vec<Want>,
}

/// Byte offsets for UTF-16 offsets: entry `n` is where the `n`th UTF-16 unit of `text` starts.
/// Telegram counts entity offsets in UTF-16 units; spans are bytes on character boundaries.
fn utf16_map(text: &str) -> Vec<u32> {
    let mut map = Vec::with_capacity(text.len() + 1);
    for (at, c) in text.char_indices() {
        // Both halves of a pair point at the start of the character, so an offset that falls
        // inside one is rounded down to a boundary.
        (0..c.len_utf16()).for_each(|_| map.push(at as u32));
    }
    map.push(text.len() as u32);
    map
}

fn place(entity: &MessageEntity) -> (i32, i32) {
    use MessageEntity as E;
    match entity {
        E::Unknown(e) => (e.offset, e.length),
        E::Mention(e) => (e.offset, e.length),
        E::Hashtag(e) => (e.offset, e.length),
        E::BotCommand(e) => (e.offset, e.length),
        E::Url(e) => (e.offset, e.length),
        E::Email(e) => (e.offset, e.length),
        E::Bold(e) => (e.offset, e.length),
        E::Italic(e) => (e.offset, e.length),
        E::Code(e) => (e.offset, e.length),
        E::Pre(e) => (e.offset, e.length),
        E::TextUrl(e) => (e.offset, e.length),
        E::MentionName(e) => (e.offset, e.length),
        E::InputMessageEntityMentionName(e) => (e.offset, e.length),
        E::Phone(e) => (e.offset, e.length),
        E::Cashtag(e) => (e.offset, e.length),
        E::Underline(e) => (e.offset, e.length),
        E::Strike(e) => (e.offset, e.length),
        E::BankCard(e) => (e.offset, e.length),
        E::Spoiler(e) => (e.offset, e.length),
        E::CustomEmoji(e) => (e.offset, e.length),
        E::Blockquote(e) => (e.offset, e.length),
        _ => (0, 0),
    }
}

/// Styled ranges from Telegram's entities. An entity that does not fit the text is dropped.
pub fn spans(text: &str, entities: &[MessageEntity]) -> Vec<Span> {
    if entities.is_empty() {
        return Vec::new();
    }
    let map = utf16_map(text);
    let mut spans = Vec::with_capacity(entities.len());
    for entity in entities {
        let (offset, length) = place(entity);
        let (Ok(from), Ok(length)) = (usize::try_from(offset), usize::try_from(length)) else {
            continue;
        };
        let (Some(&start), Some(&end)) = (map.get(from), map.get(from + length)) else {
            continue;
        };
        if start >= end {
            continue;
        }
        let said = &text[start as usize..end as usize];
        use MessageEntity as E;
        let style = match entity {
            E::Bold(_) => Style::Bold,
            E::Italic(_) => Style::Italic,
            E::Underline(_) => Style::Underline,
            E::Strike(_) => Style::Strike,
            E::Code(_) => Style::Code,
            E::Pre(pre) => Style::CodeBlock { language: pre.language.clone() },
            E::Blockquote(_) => Style::Quote,
            E::Spoiler(_) => Style::Spoiler,
            E::TextUrl(link) => Style::Link { url: link.url.clone() },
            E::Url(_) if said.contains("://") => Style::Link { url: said.to_owned() },
            E::Url(_) => Style::Link { url: format!("https://{said}") },
            E::Email(_) => Style::Link { url: format!("mailto:{said}") },
            // A mention by username names nobody the account has an id for: it opens in Telegram.
            E::Mention(_) => Style::Link { url: format!("https://t.me/{}", said.trim_start_matches('@')) },
            E::MentionName(named) => Style::Mention { kind: MentionKind::User, id: Who::User(named.user_id).text() },
            // The text under it is the plain emoji Telegram chose as its fallback.
            E::CustomEmoji(emoji) => Style::Emoji { file: MediaFile::new(format!("e{}", emoji.document_id), MediaKind::Emoji) },
            _ => continue,
        };
        spans.push(Span { start, end, style });
    }
    spans
}

/// Where `byte` sits in UTF-16 units. Both halves of a pair share a byte, so the first is used.
fn utf16_index(map: &[u32], byte: u32) -> Option<i32> {
    let index = map.iter().position(|&at| at == byte)?;
    i32::try_from(index).ok()
}

fn span_fits(text: &str, span: &Span) -> bool {
    let start = span.start as usize;
    let end = span.end as usize;
    end <= text.len() && start < end && text.is_char_boundary(start) && text.is_char_boundary(end)
}

/// Styled ranges as Telegram's entities. Offsets are UTF-16 units, the way Telegram counts.
/// A span that does not sit on the text is dropped.
pub fn entities_of(text: &str, spans: &[Span]) -> Vec<MessageEntity> {
    if spans.is_empty() {
        return Vec::new();
    }
    let map = utf16_map(text);
    let mut entities = Vec::with_capacity(spans.len());
    for span in spans {
        if !span_fits(text, span) {
            continue;
        }
        let (Some(offset), Some(end)) = (utf16_index(&map, span.start), utf16_index(&map, span.end)) else {
            continue;
        };
        let length = end - offset;
        if length <= 0 {
            continue;
        }
        let entity = match &span.style {
            Style::Bold => tl::types::MessageEntityBold { offset, length }.into(),
            Style::Italic => tl::types::MessageEntityItalic { offset, length }.into(),
            Style::Underline => tl::types::MessageEntityUnderline { offset, length }.into(),
            Style::Strike => tl::types::MessageEntityStrike { offset, length }.into(),
            Style::Code => tl::types::MessageEntityCode { offset, length }.into(),
            Style::CodeBlock { language } => tl::types::MessageEntityPre { offset, length, language: language.clone() }.into(),
            Style::Quote => tl::types::MessageEntityBlockquote { collapsed: false, offset, length }.into(),
            Style::Spoiler => tl::types::MessageEntitySpoiler { offset, length }.into(),
            Style::Link { url } => tl::types::MessageEntityTextUrl { offset, length, url: url.clone() }.into(),
            Style::Mention { kind: MentionKind::User, id } => {
                let Some(user_id) = id.strip_prefix('u').and_then(|id| id.parse().ok()) else {
                    continue;
                };
                tl::types::MessageEntityMentionName { offset, length, user_id }.into()
            }
            Style::Emoji { file } => {
                let Some(document_id) = file.id.strip_prefix('e').and_then(|id| id.parse().ok()) else {
                    continue;
                };
                tl::types::MessageEntityCustomEmoji { offset, length, document_id }.into()
            }
            _ => continue,
        };
        entities.push(entity);
    }
    entities
}

/// Spans already stored on a message, kept where they still sit on `plain`. An edit that
/// carries no markers sends these, so saving a formatted message does not wipe it.
pub fn with_stored(plain: &str, stored: &[Span]) -> Option<Vec<MessageEntity>> {
    let kept: Vec<Span> = stored.iter().filter(|span| span_fits(plain, span)).cloned().collect();
    let entities = entities_of(plain, &kept);
    (!entities.is_empty()).then_some(entities)
}

/// The text a send or a draft tells Telegram, and the entities for its markers. Telegram
/// does not read `**` itself, so the markers are taken out and named.
pub fn outgoing(text: &str) -> (String, Option<Vec<MessageEntity>>) {
    let (plain, spans) = from_markers(text);
    let entities = entities_of(&plain, &spans);
    (plain, (!entities.is_empty()).then_some(entities))
}

/// A custom emoji the catalog offered, remembered by the label the composer inserts (`:alt:`).
#[derive(Debug, Clone)]
pub(super) struct EmojiLabel {
    pub label: String,
    pub document_id: i64,
    pub fallback: String,
    /// The same label was offered for two documents, so a send cannot know which.
    pub ambiguous: bool,
}

struct EmojiSwap {
    old_start: i32,
    old_len: i32,
    new_len: i32,
    document_id: i64,
}

fn utf16_len(text: &str) -> i32 {
    text.chars().map(|c| c.len_utf16() as i32).sum()
}

fn usable_labels(labels: &[EmojiLabel]) -> Vec<&EmojiLabel> {
    let mut usable = Vec::new();
    for label in labels {
        if label.ambiguous || label.label.is_empty() || label.fallback.is_empty() {
            continue;
        }
        let clash = labels.iter().any(|other| other.label == label.label && other.document_id != label.document_id);
        if clash || usable.iter().any(|held: &&EmojiLabel| held.label == label.label) {
            continue;
        }
        usable.push(label);
    }
    usable.sort_by(|left, right| right.label.len().cmp(&left.label.len()).then(left.label.cmp(&right.label)));
    usable
}

/// `:alt:` labels the catalog remembered, swapped for the fallback glyph. When `allow` is
/// set, each swap is also a custom-emoji entity. A label that covers an existing entity is
/// dropped; entities after a swap move by how much the text shrank or grew.
pub(super) fn apply_emoji(plain: &str, entities: Vec<MessageEntity>, labels: &[EmojiLabel], allow: bool) -> (String, Vec<MessageEntity>) {
    let usable = usable_labels(labels);
    if usable.is_empty() || !plain.contains(':') {
        return (plain.to_owned(), entities);
    }
    let mut out = String::with_capacity(plain.len());
    let mut swaps = Vec::new();
    let mut from = 0usize;
    let mut old_utf16 = 0i32;
    while from < plain.len() {
        let rest = &plain[from..];
        let mut best: Option<(usize, usize)> = None;
        for (index, label) in usable.iter().enumerate() {
            if let Some(at) = rest.find(&label.label) {
                let earlier = match best {
                    Some((prev, _)) => at < prev,
                    None => true,
                };
                if earlier {
                    best = Some((at, index));
                }
            }
        }
        let Some((at, index)) = best else {
            out.push_str(rest);
            break;
        };
        let label = &usable[index];
        out.push_str(&rest[..at]);
        old_utf16 += utf16_len(&rest[..at]);
        let old_len = utf16_len(&label.label);
        let new_len = utf16_len(&label.fallback);
        out.push_str(&label.fallback);
        if old_len > 0 {
            swaps.push(EmojiSwap { old_start: old_utf16, old_len, new_len, document_id: label.document_id });
        }
        old_utf16 += old_len;
        from += at + label.label.len();
    }
    (out, shift_for_emoji(entities, &swaps, allow))
}

fn delta_before(swaps: &[EmojiSwap], at: i32) -> i32 {
    swaps.iter().filter(|swap| swap.old_start + swap.old_len <= at).map(|swap| swap.new_len - swap.old_len).sum()
}

fn shift_for_emoji(entities: Vec<MessageEntity>, swaps: &[EmojiSwap], allow: bool) -> Vec<MessageEntity> {
    if swaps.is_empty() {
        return entities;
    }
    let mut out = Vec::new();
    for entity in entities {
        let (offset, length) = place(&entity);
        if length <= 0 {
            continue;
        }
        let end = offset + length;
        let overlaps = swaps.iter().any(|swap| offset < swap.old_start + swap.old_len && end > swap.old_start);
        if overlaps {
            continue;
        }
        let delta = delta_before(swaps, offset);
        match retarget(&entity, offset + delta, length) {
            Some(next) => out.push(next),
            None if delta == 0 => out.push(entity),
            None => {}
        }
    }
    if allow {
        for swap in swaps {
            if swap.new_len <= 0 {
                continue;
            }
            let offset = swap.old_start + delta_before(swaps, swap.old_start);
            out.push(tl::types::MessageEntityCustomEmoji { offset, length: swap.new_len, document_id: swap.document_id }.into());
        }
    }
    out
}

/// The same entity at a new UTF-16 offset. Kinds a send does not produce stay as they are
/// when nothing moved, and are dropped when it did.
fn retarget(entity: &MessageEntity, offset: i32, length: i32) -> Option<MessageEntity> {
    use MessageEntity as E;
    Some(match entity {
        E::Bold(_) => tl::types::MessageEntityBold { offset, length }.into(),
        E::Italic(_) => tl::types::MessageEntityItalic { offset, length }.into(),
        E::Underline(_) => tl::types::MessageEntityUnderline { offset, length }.into(),
        E::Strike(_) => tl::types::MessageEntityStrike { offset, length }.into(),
        E::Code(_) => tl::types::MessageEntityCode { offset, length }.into(),
        E::Pre(pre) => tl::types::MessageEntityPre { offset, length, language: pre.language.clone() }.into(),
        E::Blockquote(quote) => tl::types::MessageEntityBlockquote { collapsed: quote.collapsed, offset, length }.into(),
        E::Spoiler(_) => tl::types::MessageEntitySpoiler { offset, length }.into(),
        E::TextUrl(link) => tl::types::MessageEntityTextUrl { offset, length, url: link.url.clone() }.into(),
        E::MentionName(named) => tl::types::MessageEntityMentionName { offset, length, user_id: named.user_id }.into(),
        E::CustomEmoji(emoji) => tl::types::MessageEntityCustomEmoji { offset, length, document_id: emoji.document_id }.into(),
        _ => return None,
    })
}

/// A draft's text as the composer shows it. Entities that arrived with it become markers,
/// which is what a later send turns back into entities.
pub fn draft_text(message: &str, entities: Option<&[MessageEntity]>) -> String {
    match entities {
        Some(entities) if !entities.is_empty() => to_markers(message, &spans(message, entities)),
        _ => message.to_owned(),
    }
}

/// One line for the scripted wire's log. Empty when there are no entities, so a plain send
/// stays the line it always was.
pub fn entity_note(entities: &[MessageEntity]) -> String {
    let mut note = String::new();
    for entity in entities {
        let (offset, length) = place(entity);
        match entity {
            MessageEntity::Bold(_) => note.push_str(&format!(" bold:{offset}+{length}")),
            MessageEntity::Italic(_) => note.push_str(&format!(" italic:{offset}+{length}")),
            MessageEntity::Underline(_) => note.push_str(&format!(" underline:{offset}+{length}")),
            MessageEntity::Strike(_) => note.push_str(&format!(" strike:{offset}+{length}")),
            MessageEntity::Code(_) => note.push_str(&format!(" code:{offset}+{length}")),
            MessageEntity::Pre(pre) => note.push_str(&format!(" pre:{offset}+{length}:{}", pre.language)),
            MessageEntity::Blockquote(quote) => {
                note.push_str(&format!(" quote:{offset}+{length}"));
                if quote.collapsed {
                    note.push_str(":collapsed");
                }
            }
            MessageEntity::Spoiler(_) => note.push_str(&format!(" spoiler:{offset}+{length}")),
            MessageEntity::TextUrl(link) => note.push_str(&format!(" link:{offset}+{length}:{}", link.url)),
            MessageEntity::CustomEmoji(emoji) => note.push_str(&format!(" emoji:{offset}+{length}:{}", emoji.document_id)),
            _ => {}
        }
    }
    note
}

/// Markers the composer shows, turned into spans on the text with those markers taken out.
pub fn from_markers(text: &str) -> (String, Vec<Span>) {
    if !text.contains(['*', '_', '`', '~', '|', '[', '>']) {
        return (text.to_owned(), Vec::new());
    }
    parse_markers(text, None)
}

/// Markers put back around `text`, so a draft that arrived as entities is still bold here.
pub fn to_markers(text: &str, spans: &[Span]) -> String {
    let mut events: Vec<(usize, u8, i32, i32, String)> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        let Some((open, close)) = marker_of(span) else {
            continue;
        };
        let start = span.start as usize;
        let end = span.end as usize;
        if !span_fits(text, span) {
            continue;
        }
        let len = (end - start) as i32;
        let at = i32::try_from(index).unwrap_or(0);
        events.push((start, 1, -len, -at, open));
        if !close.is_empty() {
            events.push((end, 0, len, at, close));
        }
    }
    if events.is_empty() {
        return text.to_owned();
    }
    events.sort_by(|left, right| {
        left.0.cmp(&right.0).then(left.1.cmp(&right.1)).then(left.2.cmp(&right.2)).then(left.3.cmp(&right.3))
    });
    let mut out = String::new();
    let mut at = 0;
    for (pos, _, _, _, bit) in events {
        if pos < at || pos > text.len() {
            continue;
        }
        out.push_str(&text[at..pos]);
        out.push_str(&bit);
        at = pos;
    }
    out.push_str(&text[at..]);
    out
}

fn marker_of(span: &Span) -> Option<(String, String)> {
    Some(match &span.style {
        Style::Bold => ("**".to_owned(), "**".to_owned()),
        Style::Italic => ("_".to_owned(), "_".to_owned()),
        Style::Underline => ("__".to_owned(), "__".to_owned()),
        Style::Strike => ("~~".to_owned(), "~~".to_owned()),
        Style::Code => ("`".to_owned(), "`".to_owned()),
        Style::Spoiler => ("||".to_owned(), "||".to_owned()),
        Style::CodeBlock { language } => (format!("```{language}\n"), "\n```".to_owned()),
        Style::Quote => ("> ".to_owned(), String::new()),
        Style::Link { url } => ("[".to_owned(), format!("]({url})")),
        _ => return None,
    })
}

fn parse_markers(text: &str, mut prev: Option<char>) -> (String, Vec<Span>) {
    let mut out = String::new();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let line_start = i == 0 || text.as_bytes()[i - 1] == b'\n';
        if let Some((lang, body, used)) = code_block(rest) {
            push_plain(&mut out, &mut spans, body, Style::CodeBlock { language: lang });
            i += used;
            prev = body.chars().next_back();
            continue;
        }
        if let Some((body, used)) = inline_code(rest) {
            push_plain(&mut out, &mut spans, body, Style::Code);
            i += used;
            prev = body.chars().next_back();
            continue;
        }
        if let Some((label, url, used)) = text_link(rest) {
            let start = out.len() as u32;
            absorb(&mut out, &mut spans, label);
            if (out.len() as u32) > start {
                spans.push(Span { start, end: out.len() as u32, style: Style::Link { url } });
            }
            i += used;
            prev = out.chars().next_back();
            continue;
        }
        if let Some((style, body, used)) = wrapped(rest, prev) {
            let start = out.len() as u32;
            absorb(&mut out, &mut spans, body);
            if (out.len() as u32) > start {
                spans.push(Span { start, end: out.len() as u32, style });
            }
            i += used;
            prev = out.chars().next_back();
            continue;
        }
        if line_start && rest.starts_with("> ") {
            let line = &rest[2..];
            let end = line.find('\n').unwrap_or(line.len());
            let start = out.len() as u32;
            absorb(&mut out, &mut spans, &line[..end]);
            if (out.len() as u32) > start {
                spans.push(Span { start, end: out.len() as u32, style: Style::Quote });
            }
            i += 2 + end;
            prev = out.chars().next_back();
            continue;
        }
        let ch = rest.chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
        prev = Some(ch);
    }
    (out, spans)
}

fn absorb(out: &mut String, spans: &mut Vec<Span>, body: &str) {
    let (plain, inner) = parse_markers(body, None);
    let base = out.len() as u32;
    out.push_str(&plain);
    for mut span in inner {
        span.start += base;
        span.end += base;
        spans.push(span);
    }
}

fn push_plain(out: &mut String, spans: &mut Vec<Span>, body: &str, style: Style) {
    if body.is_empty() {
        return;
    }
    let start = out.len() as u32;
    out.push_str(body);
    spans.push(Span { start, end: out.len() as u32, style });
}

fn code_block(text: &str) -> Option<(String, &str, usize)> {
    if !text.starts_with("```") {
        return None;
    }
    let rest = &text[3..];
    let nl = rest.find('\n')?;
    let lang = rest[..nl].trim().to_owned();
    let after = &rest[nl + 1..];
    let end = after.find("```")?;
    let body = after[..end].strip_suffix('\n').unwrap_or(&after[..end]);
    Some((lang, body, 3 + nl + 1 + end + 3))
}

fn inline_code(text: &str) -> Option<(&str, usize)> {
    if !text.starts_with('`') || text.starts_with("```") {
        return None;
    }
    let rest = &text[1..];
    let close = rest.find('`')?;
    if close == 0 || rest[..close].contains('\n') {
        return None;
    }
    Some((&rest[..close], close + 2))
}

fn text_link(text: &str) -> Option<(&str, String, usize)> {
    if !text.starts_with('[') {
        return None;
    }
    let rest = &text[1..];
    let label_end = rest.find("](")?;
    if label_end == 0 || rest[..label_end].contains('\n') {
        return None;
    }
    let after = &rest[label_end + 2..];
    let url_end = after.find(')')?;
    if url_end == 0 || after[..url_end].contains(char::is_whitespace) {
        return None;
    }
    Some((&rest[..label_end], after[..url_end].to_owned(), 1 + label_end + 2 + url_end + 1))
}

fn wrapped(text: &str, prev: Option<char>) -> Option<(Style, &str, usize)> {
    let pairs: &[(&str, Style, bool)] = &[
        ("||", Style::Spoiler, false),
        ("~~", Style::Strike, false),
        ("**", Style::Bold, false),
        ("__", Style::Underline, false),
        ("_", Style::Italic, true),
        ("*", Style::Italic, true),
    ];
    for (marker, style, word) in pairs {
        if !text.starts_with(marker) {
            continue;
        }
        if *word && (prev.is_some_and(|ch| ch.is_alphanumeric()) || (*marker == "*" && text.starts_with("**")) || (*marker == "_" && text.starts_with("__"))) {
            continue;
        }
        let rest = &text[marker.len()..];
        if !rest.chars().next().is_some_and(|ch| !ch.is_whitespace()) {
            continue;
        }
        let close = find_close(rest, marker, *word)?;
        let body = &rest[..close];
        if body.is_empty() {
            continue;
        }
        return Some((style.clone(), body, marker.len() * 2 + close));
    }
    None
}

fn find_close(text: &str, marker: &str, word: bool) -> Option<usize> {
    let mut at = 0;
    while let Some(rel) = text[at..].find(marker) {
        let close = at + rel;
        if close == 0 {
            at += marker.len();
            continue;
        }
        let before_ok = text[..close].chars().next_back().is_some_and(|ch| !ch.is_whitespace());
        let after_ok = !word || text[close + marker.len()..].chars().next().is_none_or(|ch| !ch.is_alphanumeric());
        if before_ok && after_ok {
            return Some(close);
        }
        at = close + marker.len();
    }
    None
}

/// A voice message's loudness: Telegram packs 5-bit values end to end; the contract wants a
/// byte a bar.
pub fn waveform(packed: &[u8]) -> Vec<u8> {
    let bars = packed.len() * 8 / 5;
    (0..bars)
        .map(|bar| {
            let bit = bar * 5;
            let (byte, shift) = (bit / 8, bit % 8);
            let pair = u16::from(packed[byte]) | u16::from(*packed.get(byte + 1).unwrap_or(&0)) << 8;
            let value = (pair >> shift) & 0x1f;
            (value * 255 / 31) as u8
        })
        .collect()
}

/// One size of a picture: its letter, width, height, bytes, and the bytes themselves if inline.
type Size<'a> = (&'a str, u32, u32, u64, Option<&'a [u8]>);

fn size_of(size: &PhotoSize) -> Option<Size<'_>> {
    Some(match size {
        PhotoSize::Size(size) => (size.r#type.as_str(), size.w as u32, size.h as u32, size.size as u64, None),
        PhotoSize::PhotoCachedSize(size) => {
            (size.r#type.as_str(), size.w as u32, size.h as u32, size.bytes.len() as u64, Some(size.bytes.as_slice()))
        }
        PhotoSize::Progressive(size) => {
            let bytes = size.sizes.iter().copied().max().unwrap_or(0) as u64;
            (size.r#type.as_str(), size.w as u32, size.h as u32, bytes, None)
        }
        // A stripped or outline placeholder is not a picture on its own.
        _ => return None,
    })
}

/// The size a row shows: the smallest whose longer side reaches `least`, else the largest.
fn pick(sizes: &[PhotoSize], least: u32) -> Option<Size<'_>> {
    let mut real: Vec<_> = sizes.iter().filter_map(size_of).collect();
    real.sort_by_key(|(_, w, h, ..)| (*w).max(*h));
    real.iter().find(|(_, w, h, ..)| (*w).max(*h) >= least).or(real.last()).copied()
}

/// What a row shows of a photo, and no more than a row needs: 320 on its longer side.
const INLINE: u32 = 320;

/// Most bytes of a stripped thumbnail's picture. Telegram's are a few hundred; one larger than
/// this is not one.
const STRIPPED_MOST: usize = 16 * 1024;

/// The JPEG header Telegram leaves out of a stripped thumbnail
/// (<https://core.telegram.org/api/files#stripped-thumbnails>). It is the same for every one but
/// the picture's height and width, at 164 and 166.
#[rustfmt::skip]
const STRIPPED_HEADER: [u8; 623] = [
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x28, 0x1c, 0x1e, 0x23, 0x1e, 0x19, 0x28,
    0x23, 0x21, 0x23, 0x2d, 0x2b, 0x28, 0x30, 0x3c, 0x64, 0x41, 0x3c, 0x37, 0x37, 0x3c, 0x7b, 0x58,
    0x5d, 0x49, 0x64, 0x91, 0x80, 0x99, 0x96, 0x8f, 0x80, 0x8c, 0x8a, 0xa0, 0xb4, 0xe6, 0xc3, 0xa0,
    0xaa, 0xda, 0xad, 0x8a, 0x8c, 0xc8, 0xff, 0xcb, 0xda, 0xee, 0xf5, 0xff, 0xff, 0xff, 0x9b, 0xc1,
    0xff, 0xff, 0xff, 0xfa, 0xff, 0xe6, 0xfd, 0xff, 0xf8, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x2b, 0x2d,
    0x2d, 0x3c, 0x35, 0x3c, 0x76, 0x41, 0x41, 0x76, 0xf8, 0xa5, 0x8c, 0xa5, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xff, 0xc0,
    0x00, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
    0x01, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
    0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05,
    0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
    0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23,
    0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
    0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a,
    0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
    0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
    0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7,
    0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5,
    0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1,
    0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xc4, 0x00, 0x1f, 0x01, 0x00, 0x03,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00,
    0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00,
    0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13,
    0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15,
    0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27,
    0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6,
    0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4,
    0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9,
    0xfa, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00,
];

/// A stripped thumbnail put back together as the JPEG it was, with its width and height.
/// `None` for bytes that are not one: another version, no size, no picture, too many bytes.
/// chatd only joins the pieces and never decodes them; a shell that cannot read the result
/// draws the usual placeholder.
pub(super) fn stripped_jpeg(stripped: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let [1, height, width, body @ ..] = stripped else {
        return None;
    };
    if *height == 0 || *width == 0 || body.is_empty() || body.len() > STRIPPED_MOST {
        return None;
    }
    let mut jpeg = Vec::with_capacity(STRIPPED_HEADER.len() + body.len() + 2);
    jpeg.extend_from_slice(&STRIPPED_HEADER);
    jpeg[164] = *height;
    jpeg[166] = *width;
    jpeg.extend_from_slice(body);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    Some((u32::from(*width), u32::from(*height), jpeg))
}

fn photo(photo: &tl::types::Photo, spoiler: bool, wants: &mut Vec<Want>) -> Option<Attachment> {
    let location = |kind: &str| -> tl::enums::InputFileLocation {
        tl::types::InputPhotoFileLocation {
            id: photo.id,
            access_hash: photo.access_hash,
            file_reference: photo.file_reference.clone(),
            thumb_size: kind.to_owned(),
        }
        .into()
    };
    let (kind, width, height, bytes, inline) = pick(&photo.sizes, INLINE)?;
    // The few pixels Telegram sends inside the message: on disk with the page, and drawn until
    // the row's own size is. A row's size that came inside the message needs none.
    let stripped = photo.sizes.iter().find_map(|size| match size {
        PhotoSize::PhotoStrippedSize(stripped) if inline.is_none() => Some(stripped),
        _ => None,
    });
    let preview = stripped.and_then(|stripped| {
        let (width, height, jpeg) = stripped_jpeg(&stripped.bytes)?;
        let mut preview = MediaFile::new(format!("p{}{}", photo.id, stripped.r#type), MediaKind::Image);
        (preview.width, preview.height, preview.bytes) = (width, height, jpeg.len() as u64);
        let location = location(&stripped.r#type);
        wants.push(Want { file: preview.clone(), dc: photo.dc_id, location, inline: Some(jpeg), row: None, watched: false, original: false });
        Some(preview)
    });
    let mut thumb = MediaFile::new(format!("p{}{kind}", photo.id), MediaKind::Image);
    (thumb.width, thumb.height, thumb.bytes) = (width, height, bytes);
    wants.push(Want { file: thumb.clone(), dc: photo.dc_id, location: location(kind), inline: inline.map(<[u8]>::to_vec), row: None, watched: false, original: false });
    // The picture itself is the largest size Telegram keeps: the viewer fills a screen with
    // it, at the screen's own pixels.
    let mut file = thumb.clone();
    if let Some((kind, width, height, bytes, inline)) = pick(&photo.sizes, u32::MAX).filter(|(largest, ..)| *largest != kind) {
        file.id = format!("p{}{kind}", photo.id);
        (file.width, file.height, file.bytes) = (width, height, bytes);
        wants.push(Want { file: file.clone(), dc: photo.dc_id, location: location(kind), inline: inline.map(<[u8]>::to_vec), row: None, watched: false, original: true });
    }
    let mut attachment = Attachment::new("Photo", file);
    attachment.thumb = Some(thumb);
    attachment.preview = preview;
    attachment.mime = "image/jpeg".to_owned();
    attachment.spoiler = spoiler;
    Some(attachment)
}

pub(super) fn document(media: &tl::types::MessageMediaDocument, document: &tl::types::Document, wants: &mut Vec<Want>) -> Attachment {
    let (mut name, mut size, mut duration, mut wave) = (String::new(), (0u32, 0u32), 0u32, Vec::new());
    let (mut sticker, mut animated, mut video, mut round, mut voice, mut audio) = (false, false, false, false, false, false);
    for attribute in &document.attributes {
        match attribute {
            DocumentAttribute::Filename(file) => name = file.file_name.clone(),
            DocumentAttribute::ImageSize(image) => size = (image.w as u32, image.h as u32),
            DocumentAttribute::Sticker(_) | DocumentAttribute::CustomEmoji(_) => sticker = true,
            DocumentAttribute::Animated => animated = true,
            DocumentAttribute::Video(attribute) => {
                video = true;
                round = attribute.round_message;
                size = (attribute.w as u32, attribute.h as u32);
                duration = attribute.duration.max(0.) as u32;
            }
            DocumentAttribute::Audio(attribute) => {
                audio = true;
                voice = attribute.voice;
                duration = attribute.duration.max(0) as u32;
                wave = attribute.waveform.as_deref().map(waveform).unwrap_or_default();
                if name.is_empty() {
                    name = attribute.title.clone().unwrap_or_default();
                }
            }
            _ => {}
        }
    }
    // A still of it, where Telegram made one: what a row shows of a video, a GIF or a sticker.
    let still = document.thumbs.as_deref().and_then(|thumbs| pick(thumbs, INLINE)).map(|(kind, width, height, bytes, inline)| {
        let mut thumb = MediaFile::new(format!("d{}{kind}", document.id), MediaKind::Image);
        (thumb.width, thumb.height, thumb.bytes) = (width, height, bytes);
        wants.push(Want {
            file: thumb.clone(),
            dc: document.dc_id,
            location: tl::types::InputDocumentFileLocation {
                id: document.id,
                access_hash: document.access_hash,
                file_reference: document.file_reference.clone(),
                thumb_size: kind.to_owned(),
            }
            .into(),
            inline: inline.map(<[u8]>::to_vec),
            row: None,
            watched: false,
            original: false,
        });
        thumb
    });
    let mime = document.mime_type.as_str();
    let kind = match () {
        _ if sticker => MediaKind::Image,
        _ if animated => MediaKind::Gif,
        _ if video => MediaKind::Video,
        _ if audio => MediaKind::Audio,
        _ if mime.starts_with("image/") => MediaKind::Image,
        _ => MediaKind::File,
    };
    let label = match () {
        _ if sticker => "Sticker",
        _ if voice || media.voice => "Voice message",
        _ if round || media.round => "Video message",
        _ if animated => "GIF",
        _ if video => "Video",
        _ if audio => "Audio",
        _ => "File",
    };
    let mut file = MediaFile::new(format!("d{}", document.id), kind);
    (file.width, file.height) = size;
    file.bytes = document.size.max(0) as u64;
    // An animated or video sticker is shown as its still; a static one is small enough to be
    // its own still, but only the thumbnail is fetched for a row either way.
    if let (true, Some(still)) = (sticker, &still) {
        file = still.clone();
    } else if kind != MediaKind::Image || still.is_some() {
        // A picture sent as a file opens in the viewer like a photo, so it is kept the same way;
        // so is anything else, which is fetched only when the user plays or opens it.
        let location = tl::types::InputDocumentFileLocation {
            id: document.id,
            access_hash: document.access_hash,
            file_reference: document.file_reference.clone(),
            thumb_size: String::new(),
        };
        wants.push(Want { file: file.clone(), dc: document.dc_id, location: location.into(), inline: None, row: None, watched: false, original: true });
    }
    let mut attachment = Attachment::new(if name.is_empty() { label.to_owned() } else { name }, file);
    attachment.thumb = still;
    attachment.mime = mime.to_owned();
    attachment.duration = duration;
    attachment.spoiler = media.spoiler;
    attachment.sticker = sticker;
    attachment.waveform = wave;
    attachment.round = round || media.round;
    attachment
}

fn plain(text: &tl::enums::TextWithEntities) -> &str {
    let tl::enums::TextWithEntities::Entities(text) = text;
    &text.text
}

/// What stands in for media tinyclient cannot draw. Never empty.
fn unsupported(media: &MessageMedia) -> Option<String> {
    Some(match media {
        MessageMedia::Poll(media) => {
            let tl::enums::Poll::Poll(poll) = &media.poll;
            format!("Poll: {}", plain(&poll.question))
        }
        MessageMedia::Geo(_) => "Location".to_owned(),
        MessageMedia::GeoLive(_) => "Live location".to_owned(),
        MessageMedia::Venue(venue) => format!("Location: {}", venue.title),
        MessageMedia::Contact(contact) => format!("Contact: {} {}", contact.first_name, contact.last_name).trim_end().to_owned(),
        MessageMedia::Game(media) => {
            let tl::enums::Game::Game(game) = &media.game;
            format!("Game: {}", game.title)
        }
        MessageMedia::Invoice(invoice) => format!("Invoice: {}", invoice.title),
        MessageMedia::Story(_) => "Story".to_owned(),
        MessageMedia::Giveaway(_) | MessageMedia::GiveawayResults(_) => "Giveaway".to_owned(),
        MessageMedia::PaidMedia(_) => "Paid media".to_owned(),
        MessageMedia::ToDo(_) => "Checklist".to_owned(),
        MessageMedia::VideoStream(_) => "Live stream".to_owned(),
        MessageMedia::Unsupported => "A message this version of tinyclient cannot show".to_owned(),
        // A photo or document that has run out (a self-destructing one, once seen).
        MessageMedia::Photo(photo) if photo.photo.is_none() => "Photo that has expired".to_owned(),
        MessageMedia::Document(document) if document.document.is_none() => "File that has expired".to_owned(),
        _ => return None,
    })
}

fn reactions(reactions: &tl::enums::MessageReactions) -> Vec<Reaction> {
    let tl::enums::MessageReactions::Reactions(reactions) = reactions;
    let one = |count: &tl::enums::ReactionCount| {
        let tl::enums::ReactionCount::Count(count) = count;
        let (emoji, file) = match &count.reaction {
            tl::enums::Reaction::Emoji(emoji) => (emoji.emoticon.clone(), None),
            tl::enums::Reaction::CustomEmoji(custom) => {
                (":emoji:".to_owned(), Some(MediaFile::new(format!("e{}", custom.document_id), MediaKind::Emoji)))
            }
            tl::enums::Reaction::Paid => ("⭐".to_owned(), None),
            tl::enums::Reaction::Empty => return None,
        };
        Some(Reaction {
            emoji,
            file,
            count: count.count.max(0) as u32,
            mine: count.chosen_order.is_some(),
        })
    };
    reactions.results.iter().filter_map(one).collect()
}

/// The reactions of a message as an update carries them, to put on the stored message.
pub fn reactions_of(update: &tl::enums::MessageReactions) -> Vec<Reaction> {
    reactions(update)
}

/// The custom-emoji documents a message's reactions name, their ids parsed from the media
/// they carry (`e<document id>`): Telegram keeps access hash and bytes apart from the
/// reaction, so chatd has to ask `messages.getCustomEmojiDocuments` for those first.
pub fn reaction_emoji_ids(reactions: &[Reaction]) -> Vec<i64> {
    reactions.iter().filter_map(|reaction| {
        reaction
            .file
            .as_ref()
            .filter(|file| file.kind == MediaKind::Emoji)
            .and_then(|file| file.id[1..].parse().ok())
    }).collect()
}

/// The custom-emoji documents a message's own text names, read from the spans' files
/// (`e<document id>`, the name a reaction's file carries too). The text under a span is the
/// alt glyph Telegram chose; Telegram keeps the document's access hash and bytes apart from the
/// entity, so chatd asks `messages.getCustomEmojiDocuments` for these ids first.
pub fn text_emoji_ids(message: &Message) -> Vec<i64> {
    message.spans.iter().filter_map(|span| match &span.style {
        Style::Emoji { file } if file.kind == MediaKind::Emoji => file.id.strip_prefix('e')?.parse().ok(),
        _ => None,
    }).collect()
}

fn sender(dir: &Directory, who: Who) -> Sender {
    let known = dir.get(who);
    Sender {
        id: who.text(),
        name: dir.name(who),
        avatar: known.and_then(|known| known.avatar()).map(|id| MediaFile::new(id, MediaKind::Avatar)),
        bot: known.is_some_and(|known| known.bot),
    }
}

fn forwarded(dir: &Directory, header: &tl::enums::MessageFwdHeader) -> Forwarded {
    let tl::enums::MessageFwdHeader::Header(header) = header;
    let from = header.from_id.as_ref().map(Who::of);
    let title = match (from, &header.from_name) {
        (Some(from), _) => dir.name(from),
        (None, Some(name)) => name.clone(),
        (None, None) => "Hidden account".to_owned(),
    };
    let link = match (from, header.channel_post) {
        (Some(from @ Who::Channel(_)), Some(post)) => message_link(from, dir.get(from), post),
        _ => String::new(),
    };
    Forwarded {
        from_title: title,
        from_space: String::new(),
        link,
    }
}

fn duration_words(seconds: i32) -> String {
    match seconds {
        ..=59 => format!("{seconds} s"),
        _ => format!("{} min", (seconds + 30) / 60),
    }
}

/// The first line of a pinned message, short enough for the grey line. A quote mark in it
/// becomes an apostrophe so the line keeps one pair of quotes.
fn first_words(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim().replace('"', "'");
    let mut words: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        words.push('…');
    }
    words
}

fn pin_line(who: &str, snippet: &str) -> String {
    let words = first_words(snippet);
    if words.is_empty() {
        format!("{who} pinned a message")
    } else {
        format!("{who} pinned \"{words}\"")
    }
}

/// Once the pinned message is known (this page, or the store), its first words replace the
/// bare "pinned a message". A line that already quotes something is left as it is.
pub(super) fn quote_pin(message: &mut Message) {
    if message.kind != MessageKind::System {
        return;
    }
    let Some(who) = message.text.strip_suffix(" pinned a message") else {
        return;
    };
    let Some(snippet) = message.reply_to.as_ref().map(|reply| reply.snippet.clone()) else {
        return;
    };
    let who = who.to_owned();
    message.text = pin_line(&who, &snippet);
}

fn count_words(n: i32, one: &str, many: &str) -> String {
    let n = n.max(0);
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// How long a message now stays, from Telegram's `period` in seconds.
fn kept_for(period: i32) -> String {
    if period <= 0 {
        return "off".to_owned();
    }
    let period = period as u32;
    let (n, one, many) = if period.is_multiple_of(604_800) {
        (period / 604_800, "week", "weeks")
    } else if period.is_multiple_of(86_400) {
        (period / 86_400, "day", "days")
    } else if period.is_multiple_of(3_600) {
        (period / 3_600, "hour", "hours")
    } else if period.is_multiple_of(60) {
        (period / 60, "minute", "minutes")
    } else {
        (period, "second", "seconds")
    };
    count_words(n as i32, one, many)
}

fn header_quote(header: Option<&tl::enums::MessageReplyHeader>) -> Option<String> {
    let tl::enums::MessageReplyHeader::Header(header) = header? else {
        return None;
    };
    header.quote_text.clone().filter(|text| !text.trim().is_empty())
}

/// A service message as one line, and whether the line names the event. `who` did it; "You"
/// when it was us. An unnamed event stays the fallback: the caller gives it a link into Telegram.
fn action(dir: &Directory, who: &str, action: &MessageAction) -> (String, bool) {
    let name = |id: i64| if id == dir.me { "you".to_owned() } else { dir.name(Who::User(id)) };
    let names = |ids: &[i64]| ids.iter().map(|id| name(*id)).collect::<Vec<_>>().join(", ");
    let days = |n: i32| count_words(n, "day", "days");
    use MessageAction as A;
    let named = |line: String| (line, true);
    match action {
        A::ChatCreate(a) => named(format!("{who} created the group \"{}\"", a.title)),
        A::ChannelCreate(a) => named(format!("\"{}\" was created", a.title)),
        A::ChatEditTitle(a) => named(format!("{who} renamed this to \"{}\"", a.title)),
        A::ChatEditPhoto(_) => named(format!("{who} changed the picture")),
        A::ChatDeletePhoto => named(format!("{who} removed the picture")),
        A::ChatAddUser(a) if a.users.len() == 1 && name(a.users[0]) == who.to_lowercase() => named(format!("{who} joined")),
        A::ChatAddUser(a) => named(format!("{who} added {}", names(&a.users))),
        A::ChatDeleteUser(a) if name(a.user_id) == who.to_lowercase() || dir.name(Who::User(a.user_id)) == who => named(format!("{who} left")),
        A::ChatDeleteUser(a) => named(format!("{who} removed {}", name(a.user_id))),
        A::ChatJoinedByLink(_) => named(format!("{who} joined with an invite link")),
        A::ChatJoinedByRequest => named(format!("{who} was let in")),
        A::ChatMigrateTo(_) => named("This group was upgraded to a supergroup".to_owned()),
        A::ChannelMigrateFrom(migrated) => named(format!("This group was upgraded from \"{}\"", migrated.title)),
        A::PinMessage => named(format!("{who} pinned a message")),
        A::HistoryClear => named("History was cleared".to_owned()),
        A::GameScore(game) => named(format!("{who} scored {}", game.score)),
        A::PaymentSent(paid) => named(match paid.currency.is_empty() {
            true => format!("{who} sent a payment"),
            false => format!("{who} sent a payment ({})", paid.currency),
        }),
        A::PaymentRefunded(paid) => named(match paid.currency.is_empty() {
            true => "A payment was refunded".to_owned(),
            false => format!("A {} payment was refunded", paid.currency),
        }),
        A::PhoneCall(call) => {
            let kind = if call.video { "Video call" } else { "Call" };
            named(match call.duration {
                Some(seconds) => format!("{kind}, {}", duration_words(seconds)),
                None => format!("{kind}, not answered"),
            })
        }
        A::GroupCall(call) => named(match call.duration {
            Some(seconds) => format!("Video chat ended, {}", duration_words(seconds)),
            None => format!("{who} started a video chat"),
        }),
        A::InviteToGroupCall(invite) => named(match invite.users.as_slice() {
            [] => format!("{who} invited you to the video chat"),
            users => format!("{who} invited {} to the video chat", names(users)),
        }),
        A::GroupCallScheduled(_) => named(format!("{who} scheduled a video chat")),
        A::ScreenshotTaken => named(format!("{who} took a screenshot")),
        A::ContactSignUp => named(format!("{who} joined Telegram")),
        A::TopicCreate(topic) => named(format!("{who} created the topic \"{}\"", topic.title)),
        A::TopicEdit(edit) => named(match (&edit.title, edit.closed) {
            (Some(title), _) => format!("{who} renamed the topic to \"{title}\""),
            (None, Some(true)) => format!("{who} closed the topic"),
            (None, Some(false)) => format!("{who} reopened the topic"),
            (None, None) => format!("{who} changed the topic"),
        }),
        A::SetMessagesTtl(ttl) => named(match ttl.period {
            0 => format!("{who} turned the auto-delete timer off"),
            period => format!("{who} set messages to auto-delete after {}", kept_for(period)),
        }),
        A::SetChatTheme(_) => named(format!("{who} changed the chat theme")),
        A::SetChatWallPaper(paper) => named(if paper.same {
            format!("{who} set the same wallpaper")
        } else {
            format!("{who} changed the wallpaper")
        }),
        A::SuggestProfilePhoto(_) => named(format!("{who} suggested a profile photo")),
        A::CustomAction(custom) => named(custom.message.clone()),
        A::BotAllowed(allowed) => named(if allowed.attach_menu {
            format!("{who} added this bot from the attachment menu")
        } else if let Some(domain) = &allowed.domain {
            format!("{who} allowed this bot to message you on {domain}")
        } else {
            format!("{who} allowed this bot to message you")
        }),
        A::WebViewDataSent(sent) => named(match first_words(&sent.text).as_str() {
            "" => format!("{who} sent data from a mini app"),
            text => format!("{who} sent data from \"{text}\""),
        }),
        A::GiftPremium(gift) => named(format!("{who} gifted Telegram Premium for {}", days(gift.days))),
        A::GiftCode(gift) => named(if gift.via_giveaway {
            format!("A giveaway sent a Premium gift code for {}", days(gift.days))
        } else {
            format!("{who} sent a Premium gift code for {}", days(gift.days))
        }),
        A::GiftStars(gift) => named(format!(
            "{who} gifted {}",
            if gift.stars == 1 { "1 Star".to_owned() } else { format!("{} Stars", gift.stars) }
        )),
        A::StarGift(gift) => named(if gift.refunded {
            "A gift was refunded".to_owned()
        } else if gift.converted {
            format!("{who} converted a gift into Stars")
        } else if gift.name_hidden {
            "Someone sent a gift".to_owned()
        } else {
            format!("{who} sent a gift")
        }),
        A::GiveawayLaunch(launch) => named(if launch.stars.is_some() {
            format!("{who} started a Stars giveaway")
        } else {
            format!("{who} started a giveaway")
        }),
        A::GiveawayResults(results) => named(format!("A giveaway finished with {}", count_words(results.winners_count, "winner", "winners"))),
        A::BoostApply(boost) => named(if boost.boosts == 1 {
            format!("{who} boosted this channel")
        } else {
            format!("{who} applied {}", count_words(boost.boosts, "boost", "boosts"))
        }),
        A::RequestedPeer(shared) => named(match shared.peers.len() {
            0 | 1 => format!("{who} shared a chat"),
            n => format!("{who} shared {n} chats"),
        }),
        A::TodoCompletions(todo) => named(if !todo.completed.is_empty() {
            format!("{who} checked off {}", count_words(todo.completed.len() as i32, "item", "items"))
        } else {
            format!("{who} marked {} not done", count_words(todo.incompleted.len() as i32, "item", "items"))
        }),
        _ => (format!("{who} changed something tinyclient cannot show yet"), false),
    }
}

/// One message as the contract has it. `None` for an empty one (a deleted id in a list).
pub fn map(dir: &Directory, raw: &tl::enums::Message) -> Option<Mapped> {
    match raw {
        tl::enums::Message::Empty(_) => None,
        tl::enums::Message::Service(service) => {
            let peer = Who::of(&service.peer_id);
            let from = service.from_id.as_ref().map_or(peer, Who::of);
            let outgoing = service.out || from == Who::User(dir.me);
            let who = if outgoing { "You".to_owned() } else { dir.name(from) };
            let (topic, reply) = topic_of(service.reply_to.as_ref());
            let (mut text, known) = action(dir, &who, &service.action);
            let pin = matches!(service.action, MessageAction::PinMessage);
            if pin {
                if let Some(words) = header_quote(service.reply_to.as_ref()) {
                    text = pin_line(&who, &words);
                }
            }
            let mut message = Message::plain(service.id.to_string(), "", text, i64::from(service.date), outgoing);
            message.sender = sender(dir, from);
            // A named event is the grey line. One this build does not word opens in Telegram,
            // the same way a poll or a story does.
            message.kind = if known {
                MessageKind::System
            } else {
                MessageKind::Unsupported { link: message_link(peer, dir.get(peer), service.id) }
            };
            Some(Mapped {
                peer,
                id: service.id,
                topic,
                discussion_top: None,
                reply: pin.then_some(reply).flatten(),
                message,
                wants: Vec::new(),
            })
        }
        tl::enums::Message::Message(raw) => {
            let mut peer = Who::of(&raw.peer_id);
            // No author: in a direct chat it is one of the two; a channel's post is the channel's.
            let from = match (&raw.from_id, peer) {
                (Some(from), _) => Who::of(from),
                (None, Who::User(_)) if raw.out => Who::User(dir.me),
                (None, peer) => peer,
            };
            // Telegram addresses @replies copies to our own peer. They are personal
            // notifications, and must never land in Saved Messages or the source group.
            let replies = is_replies(from) && (peer == Who::User(dir.me) || peer == from);
            if replies {
                peer = from;
            }
            let outgoing = raw.out || (from == Who::User(dir.me) && peer == Who::User(dir.me));
            let mut message = Message::plain(raw.id.to_string(), "", raw.message.clone(), i64::from(raw.date), outgoing);
            // The message's own timer: Telegram stops serving it at `date + ttl_period`, and
            // the official apps drop it from their stores whether online or not. The store
            // does the same.
            message.expires_at = raw.ttl_period.filter(|ttl| *ttl > 0).map(|ttl| i64::from(raw.date) + i64::from(ttl));
            message.sender = sender(dir, from);
            message.spans = spans(&raw.message, raw.entities.as_deref().unwrap_or(&[]));
            message.edited = raw.edit_date.is_some() && !raw.edit_hide;
            message.pinned = raw.pinned;
            message.mentions_me = raw.mentioned;
            message.reactions = raw.reactions.as_ref().map(reactions).unwrap_or_default();
            // A channel post's comment section rides the post: the count alone comes with it,
            // the linked group named on the wire but not used here. The comments are read
            // under the post's own id (`messages.getReplies`, which takes the channel and
            // the post) and written into the discussion group behind it: the sends' ask of
            // them is `sync::discussion`'s.
            if let Some(tl::enums::MessageReplies::Replies(info)) = &raw.replies {
                if info.channel_id.is_some() {
                    message.thread = Some(ThreadSummary {
                        id: thread_id(peer, raw.id),
                        replies: info.replies.max(0) as u32,
                        last: 0,
                        participants: Vec::new(),
                        unread: 0,
                    });
                }
            }
            message.forwarded = raw.fwd_from.as_ref().map(|header| Box::new(forwarded(dir, header)));
            if replies {
                message.mentions_me = true;
                if let (Some(tl::enums::MessageFwdHeader::Header(forward)), Some(tl::enums::MessageReplyHeader::Header(reply))) =
                    (&raw.fwd_from, &raw.reply_to)
                {
                    if let Some(author) = forward.from_id.as_ref().map(Who::of) {
                        message.sender = sender(dir, author);
                    }
                    if let (Some(group), Some(id), Some(source)) =
                        (reply.reply_to_peer_id.as_ref().map(Who::of), forward.saved_from_msg_id, &mut message.forwarded)
                    {
                        source.link = message_link(group, dir.get(group), id);
                    }
                }
            }
            let mut wants = Vec::new();
            match &raw.media {
                None | Some(MessageMedia::Empty) => {}
                Some(MessageMedia::Photo(media)) if media.photo.is_some() => {
                    if let Some(tl::enums::Photo::Photo(picture)) = &media.photo {
                        message.attachments.extend(photo(picture, media.spoiler, &mut wants));
                    }
                }
                Some(MessageMedia::Document(media)) if media.document.is_some() => {
                    if let Some(tl::enums::Document::Document(file)) = &media.document {
                        message.attachments.push(document(media, file, &mut wants));
                    }
                }
                Some(MessageMedia::WebPage(media)) => {
                    if let tl::enums::WebPage::Page(page) = &media.webpage {
                        // The card is Telegram's own; its picture is a Telegram file and is not
                        // fetched for a row.
                        message.previews.push(Preview {
                            url: page.url.clone(),
                            site: page.site_name.clone().unwrap_or_default(),
                            title: page.title.clone().unwrap_or_default(),
                            description: page.description.clone().unwrap_or_default(),
                            image: None,
                        });
                    }
                }
                // The value is the roll everyone sees. Zero means it is still spinning, so the
                // row is the emoji alone until Telegram names the result.
                Some(MessageMedia::Dice(dice)) => {
                    message.text = match dice.value {
                        1.. => format!("{} {}", dice.emoticon, dice.value),
                        _ => dice.emoticon.clone(),
                    };
                }
                Some(media) => {
                    if let Some(label) = unsupported(media) {
                        // The caption follows the label; its styles no longer line up, so they go.
                        message.text = match message.text.is_empty() {
                            true => label,
                            false => format!("{label}\n{}", message.text),
                        };
                        message.spans.clear();
                        message.kind = MessageKind::Unsupported { link: message_link(peer, dir.get(peer), raw.id) };
                    }
                }
            }
            let (topic, reply) = topic_of(raw.reply_to.as_ref());
            let discussion_top = match &raw.reply_to {
                Some(tl::enums::MessageReplyHeader::Header(header)) if !header.forum_topic => header.reply_to_top_id,
                _ => None,
            };
            Some(Mapped {
                peer,
                id: raw.id,
                topic,
                discussion_top,
                reply: if replies { None } else { reply },
                message,
                wants,
            })
        }
    }
}

/// The topic and the answered message a reply header names. In a forum a post that answers
/// nothing still carries a header, naming its topic as if it were a reply.
fn topic_of(header: Option<&tl::enums::MessageReplyHeader>) -> (Option<i32>, Option<i32>) {
    let Some(tl::enums::MessageReplyHeader::Header(header)) = header else {
        return (None, None);
    };
    match (header.forum_topic, header.reply_to_top_id, header.reply_to_msg_id) {
        (true, Some(top), reply) => (Some(top), reply),
        (true, None, top) => (top, None),
        (false, _, reply) => (None, reply),
    }
}

/// The lab's telegram-dice check. A roll of 5 reads as the emoji and the value.
pub(super) fn prove_dice_roll() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    7i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.from_id = Some(Who::User(7_002).peer());
    message.media = Some(tl::types::MessageMediaDice { value: 5, emoticon: "\u{1F3B2}".into(), game_outcome: None }.into());
    let mapped = map(&Directory::new(7_001), &message.into()).ok_or("the roll maps")?;
    if mapped.message.text != "\u{1F3B2} 5" || !mapped.wants.is_empty() {
        return Err(format!("a dice of 5 reads as the emoji and the value, got {}", mapped.message.text));
    }
    Ok(())
}

/// The lab's telegram-service-lines check. A premium gift is named, a pin quotes the message
/// it points at, and an action this build does not word opens in Telegram.
pub(super) fn prove_service_lines() -> Result<(), String> {
    let premium = tl::types::MessageActionGiftPremium {
        currency: "USD".into(),
        amount: 500,
        days: 30,
        crypto_currency: None,
        crypto_amount: None,
        message: None,
    };
    let (line, known) = action(&Directory::new(1), "Ada", &premium.into());
    if !known || line != "Ada gifted Telegram Premium for 30 days" {
        return Err(format!("a premium gift is named, got {line}"));
    }
    let header = tl::types::MessageReplyHeader {
        reply_to_scheduled: false,
        forum_topic: false,
        quote: true,
        reply_to_ephemeral: false,
        reply_to_msg_id: Some(4),
        reply_to_peer_id: None,
        reply_from: None,
        reply_media: None,
        reply_to_top_id: None,
        quote_text: Some("Bring the rope".into()),
        quote_entities: None,
        quote_offset: None,
        todo_item_id: None,
        poll_option: None,
    };
    let service = tl::types::MessageService {
        out: false,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id: 8,
        from_id: Some(Who::User(7_002).peer()),
        peer_id: Who::User(7_002).peer(),
        saved_peer_id: None,
        reply_to: Some(header.into()),
        date: 1_700_000_000,
        action: MessageAction::PinMessage,
        reactions: None,
        ttl_period: None,
    };
    let dir = Directory::new(1);
    let pinned = map(&dir, &service.into()).ok_or("a pin maps")?;
    if pinned.message.text != "Unknown pinned \"Bring the rope\"" || pinned.reply != Some(4) || pinned.message.kind != MessageKind::System {
        return Err(format!("a pin quotes its message, got {}", pinned.message.text));
    }
    let unnamed = tl::types::MessageService {
        out: false,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id: 22,
        from_id: Some(Who::User(7_002).peer()),
        peer_id: Who::User(7_002).peer(),
        saved_peer_id: None,
        reply_to: None,
        date: 1_700_000_000,
        action: MessageAction::Empty,
        reactions: None,
        ttl_period: None,
    };
    let unknown = map(&dir, &unnamed.into()).ok_or("an unnamed action maps")?;
    match &unknown.message.kind {
        MessageKind::Unsupported { link }
            if unknown.message.text.contains("cannot show yet") && link.contains("user_id=7002") && link.contains("message_id=22") => {}
        _ => return Err(format!("an unnamed action opens in Telegram, got {}", unknown.message.text)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_tl_types::Deserializable;

    fn bold(offset: i32, length: i32) -> MessageEntity {
        tl::types::MessageEntityBold { offset, length }.into()
    }

    #[test]
    fn entity_offsets_count_utf16_units_and_spans_count_bytes() {
        // "né" is 2 units and 3 bytes; the flag is 4 units and 8 bytes; the crab 2 units, 4 bytes.
        let text = "né 🇵🇹 🦀 code";
        let entities = vec![
            bold(0, 2),
            tl::types::MessageEntityItalic { offset: 3, length: 4 }.into(),
            tl::types::MessageEntityCode { offset: 11, length: 4 }.into(),
            tl::types::MessageEntityTextUrl { offset: 8, length: 2, url: "https://crab.example".into() }.into(),
        ];
        let spans = spans(text, &entities);
        let said: Vec<(&str, &Style)> = spans.iter().map(|span| (&text[span.start as usize..span.end as usize], &span.style)).collect();
        assert_eq!(said[0], ("né", &Style::Bold));
        assert_eq!(said[1], ("🇵🇹", &Style::Italic));
        assert_eq!(said[2], ("code", &Style::Code));
        assert_eq!(said[3], ("🦀", &Style::Link { url: "https://crab.example".into() }));
    }

    #[test]
    fn an_entity_that_does_not_fit_is_dropped_and_one_inside_a_pair_is_rounded() {
        assert!(spans("hi", &[bold(1, 9), bold(-1, 2), bold(5, 1), bold(1, 0)]).is_empty());
        // Offset 1 is inside the crab's surrogate pair: it starts at the crab.
        let spans = spans("🦀!", &[bold(1, 2)]);
        assert_eq!((spans[0].start, spans[0].end), (0, 5));
    }

    #[test]
    fn links_mentions_and_blocks_are_named() {
        let text = "see t.me/x @wren and Wren\nfn main() {}\nquoted";
        let entities = vec![
            tl::types::MessageEntityUrl { offset: 4, length: 6 }.into(),
            tl::types::MessageEntityMention { offset: 11, length: 5 }.into(),
            tl::types::MessageEntityMentionName { offset: 21, length: 4, user_id: 42 }.into(),
            tl::types::MessageEntityPre { offset: 26, length: 12, language: "rust".into() }.into(),
            tl::types::MessageEntityBlockquote { collapsed: false, offset: 39, length: 6 }.into(),
            tl::types::MessageEntitySpoiler { offset: 0, length: 3 }.into(),
            tl::types::MessageEntityHashtag { offset: 0, length: 3 }.into(),
        ];
        let styles: Vec<Style> = spans(text, &entities).into_iter().map(|span| span.style).collect();
        assert_eq!(
            styles,
            vec![
                Style::Link { url: "https://t.me/x".into() },
                Style::Link { url: "https://t.me/wren".into() },
                Style::Mention { kind: MentionKind::User, id: "u42".into() },
                Style::CodeBlock { language: "rust".into() },
                Style::Quote,
                Style::Spoiler,
            ]
        );
    }

    #[test]
    fn a_waveform_is_unpacked_five_bits_a_bar() {
        // Bars 31, 0, 31, 0, 31, 0, 31, 0 packed low bit first.
        let packed = [0b0001_1111, 0b0111_1100, 0b1111_0000, 0b1100_0001, 0b0000_0111];
        assert_eq!(waveform(&packed), vec![255, 0, 255, 0, 255, 0, 255, 0]);
        assert!(waveform(&[]).is_empty());
    }

    fn a_photo(sizes: Vec<PhotoSize>) -> tl::types::Photo {
        tl::types::Photo { has_stickers: false, id: 6_016, access_hash: 1, file_reference: vec![1], date: 0, sizes, video_sizes: None, dc_id: 4 }
    }

    fn sized(kind: &str, w: i32, h: i32) -> PhotoSize {
        tl::types::PhotoSize { r#type: kind.to_owned(), w, h, size: 4_096 }.into()
    }

    fn stripped(bytes: &[u8]) -> PhotoSize {
        tl::types::PhotoStrippedSize { r#type: "i".to_owned(), bytes: bytes.to_vec() }.into()
    }

    /// A real one: the fake wire's 40 by 30 photo, as Telegram would send it.
    const RIDGE: &[u8] = include_bytes!("fake_ridge_i.bin");

    #[test]
    fn a_stripped_thumbnail_is_put_back_together_as_the_jpeg_it_was() {
        let (width, height, jpeg) = stripped_jpeg(RIDGE).unwrap();
        assert_eq!((width, height), (40, 30));
        assert_eq!(jpeg.len(), STRIPPED_HEADER.len() + RIDGE.len() - 3 + 2);
        assert_eq!(&jpeg[..4], &[0xff, 0xd8, 0xff, 0xe0], "it starts a JFIF file");
        // The frame header names the picture's size where a decoder reads it.
        assert_eq!(&jpeg[158..167], &[0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 30, 0x00, 40]);
        assert_eq!(&jpeg[STRIPPED_HEADER.len()..jpeg.len() - 2], &RIDGE[3..], "the picture follows the header untouched");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);
    }

    #[test]
    fn bytes_that_are_not_a_stripped_thumbnail_make_no_preview() {
        assert!(stripped_jpeg(&[]).is_none());
        // The fake wire's other photos carry only a size and no picture.
        assert!(stripped_jpeg(&[1, 20, 30]).is_none());
        assert!(stripped_jpeg(&[2, 30, 40, 0x12]).is_none(), "another version");
        assert!(stripped_jpeg(&[1, 0, 40, 0x12]).is_none(), "no height");
        assert!(stripped_jpeg(&[1, 30, 0, 0x12]).is_none(), "no width");
        let mut huge = vec![1, 30, 40];
        huge.resize(3 + STRIPPED_MOST + 1, 0x12);
        assert!(stripped_jpeg(&huge).is_none(), "too many bytes");
    }

    #[test]
    fn a_photo_with_a_stripped_thumbnail_has_its_preview_on_disk_with_the_page() {
        let mut wants = Vec::new();
        let attachment = photo(&a_photo(vec![stripped(RIDGE), sized("m", 320, 240), sized("y", 1_280, 960)]), false, &mut wants).unwrap();
        let preview = attachment.preview.as_ref().unwrap();
        assert_eq!((preview.id.as_str(), preview.kind, preview.width, preview.height), ("p6016i", MediaKind::Image, 40, 30));
        assert_eq!(attachment.thumb.as_ref().map(|thumb| thumb.id.as_str()), Some("p6016m"));
        // Its bytes are in the want: nothing is asked of Telegram for it.
        let kept = wants.iter().find(|want| want.file.id == "p6016i").unwrap();
        assert_eq!(kept.inline.as_deref(), Some(stripped_jpeg(RIDGE).unwrap().2.as_slice()));
        assert_eq!(kept.file.bytes, kept.inline.as_ref().unwrap().len() as u64);
        assert!(!kept.original && !kept.watched && kept.row.is_none());
        // The row's size is still fetched, and the original still waits for the viewer.
        let thumb = wants.iter().find(|want| want.file.id == "p6016m").unwrap();
        assert!(thumb.inline.is_none() && !thumb.original);
        assert!(wants.iter().any(|want| want.file.id == "p6016y" && want.original));
    }

    #[test]
    fn a_photo_without_a_usable_stripped_thumbnail_has_no_preview() {
        for sizes in [
            vec![sized("m", 320, 240)],
            vec![stripped(&[1, 20, 30]), sized("m", 320, 240)],
            vec![stripped(&[]), sized("m", 320, 240)],
        ] {
            let mut wants = Vec::new();
            let attachment = photo(&a_photo(sizes), false, &mut wants).unwrap();
            assert!(attachment.preview.is_none());
            assert!(wants.iter().all(|want| want.file.id != "p6016i"));
        }
        // A row's size that came inside the message is on disk as soon as the preview would be.
        let cached = tl::types::PhotoCachedSize { r#type: "m".to_owned(), w: 320, h: 240, bytes: vec![0xff, 0xd8, 0xff, 0xd9] };
        let mut wants = Vec::new();
        let attachment = photo(&a_photo(vec![stripped(RIDGE), cached.into()]), false, &mut wants).unwrap();
        assert!(attachment.preview.is_none());
    }

    #[test]
    fn a_premium_emoji_keeps_its_alt_glyph_and_names_its_document_for_fetching() {
        let text = "Cast off 🦀 at dawn";
        let entities = vec![tl::types::MessageEntityCustomEmoji { offset: 9, length: 2, document_id: 5_003 }.into()];
        let mut message = Message::plain("1".to_string(), "Odalys".to_string(), text.to_owned(), 0, false);
        message.spans = spans(text, &entities);
        let span = &message.spans[0];
        assert_eq!(&text[span.start as usize..span.end as usize], "🦀", "the alt glyph stays in the text");
        assert_eq!(span.style, Style::Emoji { file: MediaFile::new("e5003", MediaKind::Emoji) });
        assert_eq!(text_emoji_ids(&message), vec![5_003]);
        // An emoji of another network is not a Telegram document, so it names no id to ask for.
        message.spans[0].style = Style::Emoji { file: MediaFile::new("emoji:7001", MediaKind::Emoji) };
        assert!(text_emoji_ids(&message).is_empty());
    }

    #[test]
    fn spans_round_trip_through_entities_over_text_with_emoji() {
        // "né" is 2 units and 3 bytes; the flag is 4 units and 8 bytes; the crab 2 units, 4 bytes.
        let text = "né 🇵🇹 🦀 code";
        let entities = vec![
            bold(0, 2),
            tl::types::MessageEntityItalic { offset: 3, length: 4 }.into(),
            tl::types::MessageEntityCode { offset: 11, length: 4 }.into(),
            tl::types::MessageEntityTextUrl { offset: 8, length: 2, url: "https://crab.example".into() }.into(),
            tl::types::MessageEntitySpoiler { offset: 0, length: 2 }.into(),
        ];
        let once = spans(text, &entities);
        let back = entities_of(text, &once);
        let twice = spans(text, &back);
        assert_eq!(twice, once, "entities are the inverse of spans, in UTF-16 units");
    }

    #[test]
    fn markers_become_the_spans_telegram_will_keep() {
        let (plain, styled) = from_markers("**hi** _there_");
        assert_eq!(plain, "hi there");
        let said: Vec<(&str, &Style)> = styled.iter().map(|span| (&plain[span.start as usize..span.end as usize], &span.style)).collect();
        assert_eq!(said, vec![("hi", &Style::Bold), ("there", &Style::Italic)]);
        let entities = entities_of(&plain, &styled);
        assert_eq!(place(&entities[0]), (0, 2));
        assert!(matches!(entities[0], MessageEntity::Bold(_)));
        assert_eq!(place(&entities[1]), (3, 5));
        assert!(matches!(entities[1], MessageEntity::Italic(_)));
    }

    fn dice_message(value: i32) -> tl::enums::Message {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        7i32.serialize(&mut bytes);
        Who::User(7_002).peer().serialize(&mut bytes);
        1_700_000_000i32.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
        message.from_id = Some(Who::User(7_002).peer());
        message.media = Some(
            tl::types::MessageMediaDice {
                value,
                emoticon: "\u{1F3B2}".into(),
                game_outcome: None,
            }
            .into(),
        );
        message.into()
    }

    #[test]
    fn a_dice_roll_keeps_its_value() {
        let rolled = map(&Directory::new(7_001), &dice_message(5)).expect("the roll maps");
        assert!(rolled.wants.is_empty());
        assert_eq!(rolled.message.text, "\u{1F3B2} 5");
        let spinning = map(&Directory::new(7_001), &dice_message(0)).expect("a roll still spinning maps");
        assert_eq!(spinning.message.text, "\u{1F3B2}");
    }

    fn said(action: MessageAction) -> String {
        super::action(&Directory::new(1), "Ada", &action).0
    }

    /// Every `MessageAction` of the pinned grammers schema. A new variant fails this build
    /// until it is named here, which is how an upgrade is noticed. The fallback line stays.
    fn listed(action: &MessageAction) -> &'static str {
        use MessageAction as A;
        match action {
            A::Empty => "empty",
            A::ChatCreate(_) => "chat_create",
            A::ChatEditTitle(_) => "chat_edit_title",
            A::ChatEditPhoto(_) => "chat_edit_photo",
            A::ChatDeletePhoto => "chat_delete_photo",
            A::ChatAddUser(_) => "chat_add_user",
            A::ChatDeleteUser(_) => "chat_delete_user",
            A::ChatJoinedByLink(_) => "chat_joined_by_link",
            A::ChannelCreate(_) => "channel_create",
            A::ChatMigrateTo(_) => "chat_migrate_to",
            A::ChannelMigrateFrom(_) => "channel_migrate_from",
            A::PinMessage => "pin",
            A::HistoryClear => "history_clear",
            A::GameScore(_) => "game_score",
            A::PaymentSentMe(_) => "payment_sent_me",
            A::PaymentSent(_) => "payment_sent",
            A::PhoneCall(_) => "phone_call",
            A::ScreenshotTaken => "screenshot",
            A::CustomAction(_) => "custom",
            A::BotAllowed(_) => "bot_allowed",
            A::SecureValuesSentMe(_) => "secure_values_sent_me",
            A::SecureValuesSent(_) => "secure_values_sent",
            A::ContactSignUp => "contact_sign_up",
            A::GeoProximityReached(_) => "geo_proximity",
            A::GroupCall(_) => "group_call",
            A::InviteToGroupCall(_) => "invite_to_group_call",
            A::SetMessagesTtl(_) => "ttl",
            A::GroupCallScheduled(_) => "group_call_scheduled",
            A::SetChatTheme(_) => "chat_theme",
            A::ChatJoinedByRequest => "joined_by_request",
            A::WebViewDataSentMe(_) => "web_view_sent_me",
            A::WebViewDataSent(_) => "web_view_sent",
            A::GiftPremium(_) => "gift_premium",
            A::TopicCreate(_) => "topic_create",
            A::TopicEdit(_) => "topic_edit",
            A::SuggestProfilePhoto(_) => "suggest_photo",
            A::RequestedPeer(_) => "requested_peer",
            A::SetChatWallPaper(_) => "wallpaper",
            A::GiftCode(_) => "gift_code",
            A::GiveawayLaunch(_) => "giveaway_launch",
            A::GiveawayResults(_) => "giveaway_results",
            A::BoostApply(_) => "boost",
            A::RequestedPeerSentMe(_) => "requested_peer_sent_me",
            A::PaymentRefunded(_) => "payment_refunded",
            A::GiftStars(_) => "gift_stars",
            A::PrizeStars(_) => "prize_stars",
            A::StarGift(_) => "star_gift",
            A::StarGiftUnique(_) => "star_gift_unique",
            A::PaidMessagesRefunded(_) => "paid_messages_refunded",
            A::PaidMessagesPrice(_) => "paid_messages_price",
            A::ConferenceCall(_) => "conference_call",
            A::TodoCompletions(_) => "todo_completions",
            A::TodoAppendTasks(_) => "todo_append",
            A::SuggestedPostApproval(_) => "suggested_post_approval",
            A::SuggestedPostSuccess(_) => "suggested_post_success",
            A::SuggestedPostRefund(_) => "suggested_post_refund",
            A::GiftTon(_) => "gift_ton",
            A::SuggestBirthday(_) => "suggest_birthday",
            A::StarGiftPurchaseOffer(_) => "star_gift_offer",
            A::StarGiftPurchaseOfferDeclined(_) => "star_gift_offer_declined",
            A::NewCreatorPending(_) => "new_creator_pending",
            A::ChangeCreator(_) => "change_creator",
            A::NoForwardsToggle(_) => "no_forwards_toggle",
            A::NoForwardsRequest(_) => "no_forwards_request",
            A::PollAppendAnswer(_) => "poll_append",
            A::PollDeleteAnswer(_) => "poll_delete",
            A::ManagedBotCreated(_) => "managed_bot",
        }
    }

    fn service(id: i32, action: MessageAction, reply_to: Option<tl::enums::MessageReplyHeader>) -> tl::enums::Message {
        tl::types::MessageService {
            out: false,
            mentioned: false,
            media_unread: false,
            reactions_are_possible: false,
            silent: false,
            post: false,
            legacy: false,
            id,
            from_id: Some(Who::User(7_002).peer()),
            peer_id: Who::User(7_002).peer(),
            saved_peer_id: None,
            reply_to,
            date: 1_700_000_000,
            action,
            reactions: None,
            ttl_period: None,
        }
        .into()
    }

    fn star_gift() -> MessageAction {
        use grammers_tl_types::Serializable;
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0x313a9547u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        1i64.serialize(&mut bytes);
        0x36f8c871u32.serialize(&mut bytes);
        2i64.serialize(&mut bytes);
        10i64.serialize(&mut bytes);
        5i64.serialize(&mut bytes);
        tl::types::MessageActionStarGift::from_bytes(&bytes).expect("a star gift reads back").into()
    }

    fn wallpaper(same: bool) -> MessageAction {
        use grammers_tl_types::Serializable;
        let mut bytes = Vec::new();
        (u32::from(same)).serialize(&mut bytes);
        0xa437c3edu32.serialize(&mut bytes);
        9i64.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        3i64.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        0x36f8c871u32.serialize(&mut bytes);
        2i64.serialize(&mut bytes);
        tl::types::MessageActionSetChatWallPaper::from_bytes(&bytes).expect("a wallpaper reads back").into()
    }

    fn pin_header(quote: Option<&str>) -> tl::enums::MessageReplyHeader {
        tl::types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: false,
            quote: quote.is_some(),
            reply_to_ephemeral: false,
            reply_to_msg_id: Some(4),
            reply_to_peer_id: None,
            reply_from: None,
            reply_media: None,
            reply_to_top_id: None,
            quote_text: quote.map(str::to_owned),
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
        }
        .into()
    }

    #[test]
    fn every_message_action_of_this_layer_is_listed() {
        assert_eq!(listed(&MessageAction::Empty), "empty");
        assert_eq!(listed(&MessageAction::PinMessage), "pin");
        assert!(said(MessageAction::Empty).contains("cannot show yet"));
    }

    #[test]
    fn service_lines_name_the_event_and_a_pin_quotes_its_message() {
        let premium = tl::types::MessageActionGiftPremium {
            currency: "USD".into(),
            amount: 500,
            days: 30,
            crypto_currency: None,
            crypto_amount: None,
            message: None,
        };
        assert_eq!(said(premium.into()), "Ada gifted Telegram Premium for 30 days");
        let code = tl::types::MessageActionGiftCode {
            via_giveaway: false,
            unclaimed: false,
            boost_peer: None,
            days: 90,
            slug: "code".into(),
            currency: None,
            amount: None,
            crypto_currency: None,
            crypto_amount: None,
            message: None,
        };
        assert_eq!(said(code.into()), "Ada sent a Premium gift code for 90 days");
        let stars = tl::types::MessageActionGiftStars {
            currency: "USD".into(),
            amount: 100,
            stars: 50,
            crypto_currency: None,
            crypto_amount: None,
            transaction_id: None,
        };
        assert_eq!(said(stars.into()), "Ada gifted 50 Stars");
        let paid = tl::types::MessageActionPaymentSent {
            recurring_init: false,
            recurring_used: false,
            currency: "USD".into(),
            total_amount: 500,
            invoice_slug: None,
            subscription_until_date: None,
        };
        assert_eq!(said(paid.into()), "Ada sent a payment (USD)");
        let refunded = tl::types::MessageActionPaymentRefunded {
            peer: Who::User(7_002).peer(),
            currency: "USD".into(),
            total_amount: 500,
            payload: None,
            charge: tl::types::PaymentCharge { id: "c".into(), provider_charge_id: "p".into() }.into(),
        };
        assert_eq!(said(refunded.into()), "A USD payment was refunded");
        assert_eq!(said(tl::types::MessageActionGiveawayLaunch { stars: None }.into()), "Ada started a giveaway");
        let results = tl::types::MessageActionGiveawayResults { stars: false, winners_count: 3, unclaimed_count: 0 };
        assert_eq!(said(results.into()), "A giveaway finished with 3 winners");
        assert_eq!(said(tl::types::MessageActionBoostApply { boosts: 1 }.into()), "Ada boosted this channel");
        assert_eq!(said(tl::types::MessageActionSetChatTheme { theme: tl::types::ChatTheme { emoticon: "x".into() }.into() }.into()), "Ada changed the chat theme");
        assert_eq!(
            said(tl::types::MessageActionSuggestProfilePhoto { photo: tl::types::PhotoEmpty { id: 1 }.into() }.into()),
            "Ada suggested a profile photo"
        );
        let call = tl::types::InputGroupCall { id: 1, access_hash: 2 }.into();
        assert_eq!(
            said(tl::types::MessageActionInviteToGroupCall { call, users: vec![9] }.into()),
            "Ada invited Unknown to the video chat"
        );
        let call = tl::types::InputGroupCall { id: 1, access_hash: 2 }.into();
        assert_eq!(said(tl::types::MessageActionGroupCallScheduled { call, schedule_date: 1_700_000_000 }.into()), "Ada scheduled a video chat");
        assert_eq!(said(tl::types::MessageActionGameScore { game_id: 1, score: 40 }.into()), "Ada scored 40");
        assert_eq!(
            said(tl::types::MessageActionRequestedPeer { button_id: 1, peers: vec![Who::User(9).peer()] }.into()),
            "Ada shared a chat"
        );
        assert_eq!(said(tl::types::MessageActionWebViewDataSent { text: "Checkout".into() }.into()), "Ada sent data from \"Checkout\"");
        let allowed = tl::types::MessageActionBotAllowed {
            attach_menu: false,
            from_request: false,
            domain: None,
            app: None,
        };
        assert_eq!(said(allowed.into()), "Ada allowed this bot to message you");
        assert_eq!(
            said(tl::types::MessageActionChannelMigrateFrom { title: "Ridge".into(), chat_id: 3 }.into()),
            "This group was upgraded from \"Ridge\""
        );
        assert_eq!(
            said(tl::types::MessageActionTodoCompletions { completed: vec![1], incompleted: vec![] }.into()),
            "Ada checked off 1 item"
        );
        assert_eq!(
            said(tl::types::MessageActionSetMessagesTtl { period: 86_400, auto_setting_from: None }.into()),
            "Ada set messages to auto-delete after 1 day"
        );
        assert_eq!(said(star_gift()), "Ada sent a gift");
        assert_eq!(said(wallpaper(false)), "Ada changed the wallpaper");
        assert_eq!(said(wallpaper(true)), "Ada set the same wallpaper");

        let mut quoted = Message::plain("8", "", "Ada pinned a message", 0, false);
        quoted.kind = MessageKind::System;
        quoted.reply_to = Some(protocol::ReplyTo {
            id: "4".into(),
            sender: "Bea".into(),
            snippet: "Bring the rope tomorrow and the map from the hut please".into(),
            person: None,
            deleted: false,
        });
        quote_pin(&mut quoted);
        assert_eq!(quoted.text, "Ada pinned \"Bring the rope tomorrow and the map from…\"");

        let dir = Directory::new(1);
        let pinned = map(&dir, &service(8, MessageAction::PinMessage, Some(pin_header(Some("Bring the rope"))))).expect("a pin maps");
        assert_eq!(pinned.message.text, "Unknown pinned \"Bring the rope\"");
        assert_eq!(pinned.reply, Some(4));
        assert!(matches!(pinned.message.kind, MessageKind::System));

        let unknown = map(&dir, &service(22, MessageAction::Empty, None)).expect("an unknown action maps");
        assert!(unknown.message.text.contains("cannot show yet"));
        match unknown.message.kind {
            MessageKind::Unsupported { link } => {
                assert!(link.contains("user_id=7002"), "{link}");
                assert!(link.contains("message_id=22"), "{link}");
            }
            MessageKind::System | MessageKind::Normal => panic!("an unnamed action needs a link into Telegram"),
        }
    }
}
