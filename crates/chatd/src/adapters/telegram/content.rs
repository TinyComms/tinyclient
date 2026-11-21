//! Telegram's messages into the one message model: entities to spans, media to attachments,
//! service messages to one grey line, and whatever tinyclient cannot draw to a named line with a
//! link into Telegram's own app. Nothing here asks the network anything.

use super::peers::{is_replies, message_link, thread_id, Directory, Who};
use grammers_tl_types as tl;
use protocol::{
    Attachment, Card, Forwarded, MediaFile, MediaKind, MentionKind, Message, MessageKind, Preview, Reaction, ReplyTo, Sender, Span,
    Style, ThreadSummary,
};
use std::collections::HashSet;
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

/// What a reply header says besides the id of the message it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplyNote {
    /// The part that was quoted. Empty when Telegram did not send the words.
    pub quote: String,
    /// Who wrote it, when the header names them.
    pub author: String,
    /// `u<id>` when that author is a user. Empty when the name is all Telegram sent.
    pub author_id: String,
    /// The answered message is in another chat, so an id in this one is a different message.
    pub elsewhere: bool,
    /// The other chat's name, when the directory knows it.
    pub place: String,
    /// A reply to a story. The line says "Story" and [`ReplyNote::story_id`] is what it points at.
    pub story: bool,
    pub story_id: i32,
}

#[derive(Clone)]
pub struct Mapped {
    pub peer: Who,
    pub id: i32,
    /// The forum topic its reply header names. `None` in a forum means the General topic.
    pub topic: Option<i32>,
    /// The root of a discussion thread, distinct from the comment this message answers.
    pub discussion_top: Option<i32>,
    /// The message it answers, when it is a reply and not just a post in a topic.
    pub reply: Option<i32>,
    /// A partial quote, a reply in another chat, or a reply to a story.
    pub reply_note: Option<ReplyNote>,
    pub message: Message,
    /// What it shows inline (thumbnails and stills), and the originals of its pictures, which
    /// are only fetched when the viewer asks.
    pub wants: Vec<Want>,
    /// Telegram's `grouped_id`. Items that share one are a single album.
    pub group: Option<i64>,
    /// Every Telegram message id folded into this row, including [`Mapped::id`]. Empty when
    /// the row is one message.
    pub members: Vec<i32>,
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
            // A send names the person with an input user. The echo carries that until Telegram
            // stores the incoming form.
            E::InputMessageEntityMentionName(named) => {
                let tl::enums::InputUser::User(user) = &named.user_id else {
                    continue;
                };
                Style::Mention { kind: MentionKind::User, id: Who::User(user.user_id).text() }
            }
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

/// Explicit mention spans from the composer, turned into Telegram `InputMessageEntityMentionName`
/// entities using UTF-16 offsets on `plain`.
pub(super) fn apply_mention_spans(
    plain: &str,
    mut entities: Vec<MessageEntity>,
    spans: &[Span],
    resolve: impl Fn(i64) -> Option<i64>,
) -> Vec<MessageEntity> {
    if spans.is_empty() {
        return entities;
    }
    let map = utf16_map(plain);
    for span in spans {
        let Style::Mention { kind: MentionKind::User, id } = &span.style else {
            continue;
        };
        if !span_fits(plain, span) {
            continue;
        }
        let Some(user_id) = id.strip_prefix('u').unwrap_or(id).parse::<i64>().ok() else {
            continue;
        };
        let (Some(offset), Some(end)) = (utf16_index(&map, span.start), utf16_index(&map, span.end)) else {
            continue;
        };
        let length = end - offset;
        if length <= 0 {
            continue;
        }
        if let Some(access_hash) = resolve(user_id) {
            entities.push(
                tl::types::InputMessageEntityMentionName {
                    offset,
                    length,
                    user_id: tl::types::InputUser { user_id, access_hash }.into(),
                }
                .into(),
            );
        } else {
            entities.push(tl::types::MessageEntityMentionName { offset, length, user_id }.into());
        }
    }
    entities
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

/// Someone the directory can notify. `handle` is empty when they have no username.
#[derive(Debug, Clone)]
pub(super) struct MentionTarget {
    pub name: String,
    pub first: String,
    pub handle: String,
    pub user_id: i64,
    pub access_hash: i64,
}

struct MentionSwap {
    old_start: i32,
    old_len: i32,
    new_len: i32,
    /// Set when the replacement is a text mention. A username is plain `@handle` and notifies on its own.
    mention: Option<(i64, i64)>,
}

fn boundary_before(text: &str, at: usize) -> bool {
    text[..at].chars().next_back().is_none_or(char::is_whitespace)
}

fn boundary_after(text: &str, at: usize) -> bool {
    text[at..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_')
}

/// The next `@Display Name` that stands on its own. An earlier `@` glued to other text is skipped.
fn mention_at(plain: &str, from: usize, person: &MentionTarget) -> Option<usize> {
    let needle = format!("@{}", person.name);
    let mut search = from;
    while let Some(at) = plain[search..].find(&needle) {
        let start = search + at;
        let end = start + needle.len();
        if boundary_before(plain, start) && boundary_after(plain, end) {
            return Some(start);
        }
        search = start + 1;
    }
    None
}

/// `@Display Name` as the composer inserts it. A person with a username becomes `@username`.
/// A person without one becomes their first name, covered by a text mention Telegram notifies on.
/// Two people with the same name are left as typed. Entities that covered a replaced run are
/// dropped; ones after it move by how much the text shrank or grew.
pub(super) fn apply_mentions(plain: &str, entities: Vec<MessageEntity>, people: &[MentionTarget]) -> (String, Vec<MessageEntity>) {
    if people.is_empty() || !plain.contains('@') {
        return (plain.to_owned(), entities);
    }
    let mut people: Vec<&MentionTarget> = people.iter().filter(|person| !person.name.is_empty()).collect();
    people.sort_by(|left, right| right.name.len().cmp(&left.name.len()).then(left.name.cmp(&right.name)));
    let mut out = String::with_capacity(plain.len());
    let mut swaps = Vec::new();
    let mut from = 0usize;
    let mut old_utf16 = 0i32;
    while from < plain.len() {
        let mut best: Option<(usize, usize)> = None;
        for (index, person) in people.iter().enumerate() {
            let Some(start) = mention_at(plain, from, person) else {
                continue;
            };
            // The same place keeps the longer name, which was sorted first.
            let earlier = best.is_none_or(|(prev, _)| start < prev);
            if earlier {
                best = Some((start, index));
            }
        }
        let Some((start, index)) = best else {
            out.push_str(&plain[from..]);
            break;
        };
        let person = people[index];
        let needle_len = 1 + person.name.len();
        out.push_str(&plain[from..start]);
        old_utf16 += utf16_len(&plain[from..start]);
        let old_len = utf16_len(&plain[start..start + needle_len]);
        let (spoken, mention) = match person.handle.is_empty() {
            true => (person.first.clone(), Some((person.user_id, person.access_hash))),
            false => (format!("@{}", person.handle), None),
        };
        let new_len = utf16_len(&spoken);
        out.push_str(&spoken);
        if old_len > 0 {
            swaps.push(MentionSwap { old_start: old_utf16, old_len, new_len, mention });
        }
        old_utf16 += old_len;
        from = start + needle_len;
    }
    (out, shift_for_mentions(entities, &swaps))
}

fn mention_delta(swaps: &[MentionSwap], at: i32) -> i32 {
    swaps.iter().filter(|swap| swap.old_start + swap.old_len <= at).map(|swap| swap.new_len - swap.old_len).sum()
}

fn shift_for_mentions(entities: Vec<MessageEntity>, swaps: &[MentionSwap]) -> Vec<MessageEntity> {
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
        let delta = mention_delta(swaps, offset);
        match retarget(&entity, offset + delta, length) {
            Some(next) => out.push(next),
            None if delta == 0 => out.push(entity),
            None => {}
        }
    }
    for swap in swaps {
        let Some((user_id, access_hash)) = swap.mention else {
            continue;
        };
        if swap.new_len <= 0 {
            continue;
        }
        let offset = swap.old_start + mention_delta(swaps, swap.old_start);
        let user = tl::types::InputUser { user_id, access_hash };
        out.push(tl::types::InputMessageEntityMentionName { offset, length: swap.new_len, user_id: user.into() }.into());
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
        E::InputMessageEntityMentionName(named) => tl::types::InputMessageEntityMentionName { offset, length, user_id: named.user_id.clone() }.into(),
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
            MessageEntity::InputMessageEntityMentionName(named) => {
                let id = match &named.user_id {
                    tl::enums::InputUser::User(user) => user.user_id,
                    _ => 0,
                };
                note.push_str(&format!(" mention:{offset}+{length}:{id}"));
            }
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

/// The picture a link card shows. A photo's row-sized still, or a video's thumbnail. The full
/// picture stays with Telegram until someone opens it, and a card does not open it.
fn preview_image(page: &tl::types::WebPage, wants: &mut Vec<Want>) -> Option<MediaFile> {
    if let Some(tl::enums::Photo::Photo(photo)) = &page.photo {
        let (kind, width, height, bytes, inline) = pick(&photo.sizes, INLINE)?;
        let mut image = MediaFile::new(format!("p{}{kind}", photo.id), MediaKind::Image);
        (image.width, image.height, image.bytes) = (width, height, bytes);
        wants.push(Want {
            file: image.clone(),
            dc: photo.dc_id,
            location: tl::types::InputPhotoFileLocation {
                id: photo.id,
                access_hash: photo.access_hash,
                file_reference: photo.file_reference.clone(),
                thumb_size: kind.to_owned(),
            }
            .into(),
            inline: inline.map(<[u8]>::to_vec),
            row: None,
            watched: false,
            original: false,
        });
        return Some(image);
    }
    let tl::enums::Document::Document(document) = page.document.as_ref()? else { return None };
    let (kind, width, height, bytes, inline) = document.thumbs.as_deref().and_then(|thumbs| pick(thumbs, INLINE))?;
    let mut image = MediaFile::new(format!("d{}{kind}", document.id), MediaKind::Image);
    (image.width, image.height, image.bytes) = (width, height, bytes);
    wants.push(Want {
        file: image.clone(),
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
    Some(image)
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
    // A Lottie or WebM sticker is small (under 64 KB and 256 KB). The still stays the
    // thumbnail, and the document itself is fetched with the row. A static sticker is its
    // own still: only the thumbnail is fetched.
    let moving = sticker && matches!(mime, "application/x-tgsticker" | "video/webm");
    let location = tl::types::InputDocumentFileLocation {
        id: document.id,
        access_hash: document.access_hash,
        file_reference: document.file_reference.clone(),
        thumb_size: String::new(),
    };
    if moving {
        // `.tgs` arrives gzipped, like a custom emoji, so the fetch unpacks it for the Lottie player.
        file.kind = if mime == "application/x-tgsticker" { MediaKind::Emoji } else { MediaKind::Video };
        wants.push(Want { file: file.clone(), dc: document.dc_id, location: location.into(), inline: None, row: None, watched: false, original: false });
    } else if let (true, Some(still)) = (sticker, &still) {
        file = still.clone();
    } else if kind != MediaKind::Image || still.is_some() {
        // A picture sent as a file opens in the viewer like a photo, so it is kept the same way;
        // so is anything else, which is fetched only when the user plays or opens it.
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

/// The card for a poll or a quiz. Counts stay off while Telegram is still hiding them.
/// Voting, retracting, and creating a poll stay a hand-off: the message keeps its link.
pub(super) fn poll_card(poll: &tl::types::Poll, results: &tl::types::PollResults) -> Card {
    let hidden = results.min || (poll.hide_results_until_close && !poll.closed);
    let rows = results.results.as_deref().unwrap_or(&[]);
    let mut fields = Vec::new();
    for answer in &poll.answers {
        let tl::enums::PollAnswer::Answer(answer) = answer else { continue };
        let found = rows.iter().find_map(|one| match one {
            tl::enums::PollAnswerVoters::Voters(row) if row.option == answer.option => Some(row),
            _ => None,
        });
        let mut label = String::new();
        if !hidden {
            if let Some(count) = found.and_then(|row| row.voters) {
                label = if count == 1 { "1 vote".to_owned() } else { format!("{count} votes") };
            }
        }
        if found.is_some_and(|row| row.chosen) {
            label = if label.is_empty() { "Your vote".to_owned() } else { format!("{label} · your vote") };
        }
        if found.is_some_and(|row| row.correct) {
            label = if label.is_empty() { "Correct".to_owned() } else { format!("{label} · correct") };
        }
        if label.is_empty() {
            label = "Option".to_owned();
        }
        fields.push((label, plain(&answer.text).to_owned()));
    }
    let extra = fields.len().saturating_sub(protocol::CARD_FIELDS);
    fields.truncate(protocol::CARD_FIELDS);
    let mut lines = Vec::new();
    if poll.quiz {
        lines.push("Quiz".to_owned());
    }
    if poll.closed {
        lines.push("Closed".to_owned());
    }
    if poll.multiple_choice {
        lines.push("Pick any".to_owned());
    }
    if poll.public_voters {
        lines.push("Votes are public".to_owned());
    }
    if hidden {
        lines.push(
            if poll.hide_results_until_close && !poll.closed {
                "Results stay hidden until the poll closes."
            } else {
                "Results stay hidden until you vote."
            }
            .to_owned(),
        );
    } else if let Some(total) = results.total_voters {
        lines.push(if total == 1 { "1 vote".to_owned() } else { format!("{total} votes") });
    }
    if extra > 0 {
        lines.push(format!("{extra} more"));
    }
    if let Some(solution) = results.solution.as_ref().filter(|text| !text.is_empty()) {
        lines.push(solution.clone());
    }
    Card {
        title: plain(&poll.question).to_owned(),
        text: lines.join("\n"),
        fields,
        footer: "Open in Telegram".to_owned(),
        ..Card::default()
    }
}

fn plain(text: &tl::enums::TextWithEntities) -> &str {
    let tl::enums::TextWithEntities::Entities(text) = text;
    &text.text
}

/// View-once is Telegram's `ttl_seconds` of `0x7FFFFFFF`. Anything shorter is a timer.
fn locked_media(kind: &str, ttl: Option<i32>) -> String {
    if ttl == Some(i32::MAX) {
        return format!("View-once {kind}");
    }
    let named = match kind {
        "photo" => "Photo",
        "video" => "Video",
        "voice message" => "Voice message",
        "video message" => "Video message",
        _ => "File",
    };
    format!("{named} with a timer")
}

/// Voice, a round video, a video, or a file, from the media flags and then the document.
fn locked_kind(media: &tl::types::MessageMediaDocument) -> &'static str {
    if media.voice {
        return "voice message";
    }
    if media.round {
        return "video message";
    }
    if media.video {
        return "video";
    }
    let Some(tl::enums::Document::Document(file)) = &media.document else {
        return "file";
    };
    let mut kind = "file";
    for attribute in &file.attributes {
        match attribute {
            DocumentAttribute::Audio(audio) if audio.voice => kind = "voice message",
            DocumentAttribute::Video(video) if video.round_message => kind = "video message",
            DocumentAttribute::Video(_) if kind == "file" => kind = "video",
            _ => {}
        }
    }
    kind
}

/// A checklist as text. The title is an ordinary line. Each item is a list line, checked
/// when a completion names its id. Ticking stays in Telegram, so the last line is `link`
/// (the same address the old box opened) and the row can still reach that message.
fn checklist(media: &tl::types::MessageMediaToDo, link: &str) -> (String, Vec<Span>) {
    let tl::enums::TodoList::List(list) = &media.todo;
    let done = |id: i32| {
        media.completions.as_ref().is_some_and(|all| {
            all.iter().any(|one| {
                let tl::enums::TodoCompletion::Completion(one) = one;
                one.id == id
            })
        })
    };
    let mut text = plain(&list.title).to_owned();
    let mut spans = Vec::new();
    for item in &list.list {
        let tl::enums::TodoItem::Item(item) = item;
        let mark = if done(item.id) { "\u{2611}" } else { "\u{2610}" };
        if !text.is_empty() {
            text.push('\n');
        }
        let start = text.len() as u32;
        text.push_str(mark);
        text.push(' ');
        text.push_str(plain(&item.title));
        spans.push(Span { start, end: text.len() as u32, style: Style::ListItem { ordered: false, depth: 0 } });
    }
    if !link.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        let start = text.len() as u32;
        text.push_str("Open in Telegram");
        spans.push(Span { start, end: text.len() as u32, style: Style::Link { url: link.to_owned() } });
    }
    (text, spans)
}

/// What stands in for media tinyclient cannot draw. Never empty.
fn unsupported(media: &MessageMedia) -> Option<String> {
    Some(match media {
        MessageMedia::Poll(media) => {
            let tl::enums::Poll::Poll(poll) = &media.poll;
            format!("Poll: {}", plain(&poll.question))
        }
        MessageMedia::Game(media) => {
            let tl::enums::Game::Game(game) = &media.game;
            format!("Game: {}", game.title)
        }
        MessageMedia::Invoice(invoice) => format!("Invoice: {}", invoice.title),
        MessageMedia::Story(_) => "Story".to_owned(),
        MessageMedia::Giveaway(_) | MessageMedia::GiveawayResults(_) => "Giveaway".to_owned(),
        MessageMedia::PaidMedia(_) => "Paid media".to_owned(),
        MessageMedia::VideoStream(_) => "Live stream".to_owned(),
        MessageMedia::Unsupported => "A message this version of tinyclient cannot show".to_owned(),
        // A photo or document that has run out (a self-destructing one, once seen).
        MessageMedia::Photo(photo) if photo.photo.is_none() => "Photo that has expired".to_owned(),
        MessageMedia::Document(document) if document.document.is_none() => "File that has expired".to_owned(),
        // Still on Telegram's servers, but with a timer. Desktop does not open these. Fetching
        // them would keep a file the sender was promised would not stay.
        MessageMedia::Photo(photo) if photo.ttl_seconds.is_some() => locked_media("photo", photo.ttl_seconds),
        MessageMedia::Document(document) if document.ttl_seconds.is_some() => locked_media(locked_kind(document), document.ttl_seconds),
        _ => return None,
    })
}

/// Stars, as a count. One is singular.
fn stars(amount: i64) -> String {
    if amount == 1 {
        "1 Star".to_owned()
    } else {
        format!("{amount} Stars")
    }
}

/// How many decimal places the currency's minor unit has. Stars are not a currency here.
fn minor_digits(currency: &str) -> u32 {
    match currency.to_ascii_uppercase().as_str() {
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX" | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        _ => 2,
    }
}

/// A price in the network's own words: `USD 5.00`, `50 Stars`. Empty when there is no currency.
fn price_of(currency: &str, amount: i64) -> Option<String> {
    if currency.is_empty() {
        return None;
    }
    if currency.eq_ignore_ascii_case("XTR") {
        return Some(stars(amount));
    }
    let digits = minor_digits(currency);
    let scale = 10i64.pow(digits);
    let sign = if amount < 0 { "-" } else { "" };
    let amount = amount.abs();
    let code = currency.to_ascii_uppercase();
    let (whole, frac) = (amount / scale, amount % scale);
    Some(if digits == 0 {
        format!("{sign}{code} {whole}")
    } else {
        format!("{sign}{code} {whole}.{frac:0width$}", width = digits as usize)
    })
}

/// A coordinate with trailing zeros trimmed, so `37.774900` reads `37.7749`.
fn coord(value: f64) -> String {
    let text = format!("{value:.6}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" { "0".to_owned() } else { trimmed.to_owned() }
}

/// `geo:lat,lon`, which a maps app opens. Nothing here asks a tile server.
fn geo_uri(lat: f64, long: f64) -> String {
    format!("geo:{},{}", coord(lat), coord(long))
}

fn coordinates(geo: &tl::enums::GeoPoint) -> Option<(f64, f64, Option<i32>)> {
    match geo {
        tl::enums::GeoPoint::Point(point) => Some((point.lat, point.long, point.accuracy_radius)),
        tl::enums::GeoPoint::Empty => None,
    }
}

fn place_fields(lat: f64, long: f64, accuracy: Option<i32>) -> Vec<(String, String)> {
    let mut fields = vec![("Latitude".to_owned(), coord(lat)), ("Longitude".to_owned(), coord(long))];
    if let Some(meters) = accuracy.filter(|meters| *meters > 0) {
        fields.push(("Accuracy".to_owned(), format!("{meters} m")));
    }
    fields
}

/// Still shared when the share's end is ahead of now. This layer has no stopped flag.
fn still_sharing(date: i32, period: i32) -> bool {
    if period <= 0 {
        return false;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    i64::from(date) + i64::from(period) > now
}

/// A shared place: the name, the coordinates, and a `geo:` link. A live one says how long it
/// was shared for and whether it is still going. The shell draws the card; this build does not
/// fetch a map tile.
fn location_card(media: &MessageMedia, date: i32) -> Option<Card> {
    let mut card = Card { footer: "Open in Maps".to_owned(), ..Card::default() };
    match media {
        MessageMedia::Geo(media) => {
            card.title = "Location".to_owned();
            match coordinates(&media.geo) {
                Some((lat, long, accuracy)) => {
                    card.text = geo_uri(lat, long);
                    card.fields = place_fields(lat, long, accuracy);
                }
                None => card.text = "Coordinates were not included".to_owned(),
            }
        }
        MessageMedia::Venue(venue) => {
            card.title = if venue.title.is_empty() { "Location".to_owned() } else { venue.title.clone() };
            card.text = venue.address.clone();
            if let Some((lat, long, accuracy)) = coordinates(&venue.geo) {
                card.fields = place_fields(lat, long, accuracy);
                if card.text.is_empty() {
                    card.text = geo_uri(lat, long);
                }
            }
        }
        MessageMedia::GeoLive(live) => {
            card.title = "Live location".to_owned();
            if let Some((lat, long, accuracy)) = coordinates(&live.geo) {
                card.text = geo_uri(lat, long);
                card.fields = place_fields(lat, long, accuracy);
            } else {
                card.text = "Coordinates were not included".to_owned();
            }
            if let Some(heading) = live.heading {
                card.fields.push(("Heading".to_owned(), format!("{heading}°")));
            }
            if live.period > 0 {
                card.fields.push(("Shared for".to_owned(), kept_for(live.period)));
            }
            let status = if still_sharing(date, live.period) { "Still sharing" } else { "Stopped" };
            card.fields.push(("Status".to_owned(), status.to_owned()));
        }
        _ => return None,
    }
    Some(card)
}

/// Parses a Telegram `MessageMediaContact` into a structured `SharedContact`, preserving
/// phone number (normalized with `+` when digits), vCard email/organization, and `user_id`.
fn shared_contact(contact: &tl::types::MessageMediaContact) -> protocol::SharedContact {
    let first = contact.first_name.trim();
    let last = contact.last_name.trim();
    let name = match (first.is_empty(), last.is_empty()) {
        (false, false) => format!("{first} {last}"),
        (false, true) => first.to_owned(),
        (true, false) => last.to_owned(),
        (true, true) => String::new(),
    };
    let phone = contact.phone_number.trim();
    let phones = if phone.is_empty() {
        Vec::new()
    } else if phone.starts_with('+') {
        vec![phone.to_owned()]
    } else if phone.bytes().all(|b| b.is_ascii_digit() || matches!(b, b' ' | b'-' | b'(' | b')')) {
        vec![format!("+{phone}")]
    } else {
        vec![phone.to_owned()]
    };
    let mut emails = Vec::new();
    let mut organization = String::new();
    for line in contact.vcard.lines() {
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let prop = key.split(';').next().unwrap_or("").trim().to_ascii_uppercase();
        let val = value.trim();
        if val.is_empty() {
            continue;
        }
        match prop.as_str() {
            "EMAIL" => {
                if !emails.iter().any(|e: &String| e.eq_ignore_ascii_case(val)) {
                    emails.push(val.to_owned());
                }
            }
            "ORG" if organization.is_empty() => {
                organization = val.trim_end_matches(';').replace(';', " · ").trim().to_owned();
            }
            _ => {}
        }
    }
    let person_id = (contact.user_id != 0).then(|| Who::User(contact.user_id).text());
    protocol::SharedContact {
        name,
        phones,
        emails,
        organization,
        avatar: None,
        person_id,
    }
}

/// The card for an invoice or a paid-media preview. The message stays a hand-off: paying and
/// buying Stars stay in Telegram's own app, and no card number is read or stored here.
fn detail_card(media: &MessageMedia) -> Option<Card> {
    match media {
        MessageMedia::Invoice(invoice) => {
            let mut card = Card {
                title: invoice.title.clone(),
                text: invoice.description.clone(),
                footer: if invoice.receipt_msg_id.is_some() {
                    "Paid · Open in Telegram".to_owned()
                } else {
                    "Open in Telegram".to_owned()
                },
                ..Card::default()
            };
            if let Some(price) = price_of(&invoice.currency, invoice.total_amount) {
                card.fields.push(("Price".to_owned(), price));
            }
            if invoice.test {
                card.fields.push(("Mode".to_owned(), "Test".to_owned()));
            }
            Some(card)
        }
        MessageMedia::PaidMedia(paid) => {
            let video = paid.extended_media.iter().any(|one| {
                matches!(one, tl::enums::MessageExtendedMedia::Preview(preview) if preview.video_duration.is_some())
            });
            let unlocked = paid.extended_media.iter().any(|one| matches!(one, tl::enums::MessageExtendedMedia::Media(_)));
            let text = if unlocked {
                "Unlocked.".to_owned()
            } else if video {
                "Locked video. The preview stays blurred until it is paid for.".to_owned()
            } else {
                "Locked. The preview stays blurred until it is paid for.".to_owned()
            };
            let mut card = Card {
                title: "Paid media".to_owned(),
                text,
                footer: "Open in Telegram".to_owned(),
                ..Card::default()
            };
            card.fields.push(("Price".to_owned(), stars(paid.stars_amount)));
            Some(card)
        }
        _ => None,
    }
}

fn reactions(reactions: &tl::enums::MessageReactions) -> Vec<Reaction> {
    let tl::enums::MessageReactions::Reactions(reactions) = reactions;
    // Telegram hides who reacted in channels and large groups: `can_see_list` unset, or a
    // `min` message that carries only part of it. There the chip stays a number and says so.
    let listed = !reactions.min && reactions.can_see_list;
    let same = |a: &tl::enums::Reaction, b: &tl::enums::Reaction| match (a, b) {
        (tl::enums::Reaction::Emoji(a), tl::enums::Reaction::Emoji(b)) => a.emoticon == b.emoticon,
        (tl::enums::Reaction::CustomEmoji(a), tl::enums::Reaction::CustomEmoji(b)) => a.document_id == b.document_id,
        (tl::enums::Reaction::Paid, tl::enums::Reaction::Paid) => true,
        _ => false,
    };
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
        // The list Telegram already sent with the message names each reactor's peer.
        let who = listed
            .then(|| {
                reactions
                    .recent_reactions
                    .iter()
                    .flatten()
                    .filter_map(|reacted| match reacted {
                        tl::enums::MessagePeerReaction::Reaction(reacted) => Some(reacted),
                    })
                    .filter(|reacted| same(&reacted.reaction, &count.reaction))
                    .map(|reacted| Who::of(&reacted.peer_id).text())
                    .take(protocol::REACTION_REACTORS)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(Reaction {
            emoji,
            file,
            count: count.count.max(0) as u32,
            mine: count.chosen_order.is_some(),
            who,
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
        A::PaymentSent(paid) => named(match price_of(&paid.currency, paid.total_amount) {
            Some(price) => format!("{who} sent a payment of {price}"),
            None => format!("{who} sent a payment"),
        }),
        A::PaymentRefunded(paid) => named(match price_of(&paid.currency, paid.total_amount) {
            Some(price) => format!("A payment of {price} was refunded"),
            None => "A payment was refunded".to_owned(),
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
        A::GeoProximityReached(reached) => {
            let side = |peer: &tl::enums::Peer| {
                let person = Who::of(peer);
                if person == Who::User(dir.me) { "you".to_owned() } else { dir.name(person) }
            };
            let meters = if reached.distance == 1 { "1 m".to_owned() } else { format!("{} m", reached.distance) };
            named(format!("{} is within {meters} of {}", side(&reached.from_id), side(&reached.to_id)))
        }
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
                reply_note: None,
                message,
                wants: Vec::new(),
                group: None,
                members: Vec::new(),
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
            message.edited_at = if raw.edit_hide { None } else { raw.edit_date.map(i64::from) };
            message.edited = message.edited_at.is_some();
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
            // An inline result names the bot it came through. The handle is whatever this
            // account already knows; mapping a message does not ask the network.
            if let Some(bot) = raw.via_bot_id {
                if let Some(handle) = dir.get(Who::User(bot)).map(|known| known.handle.as_str()).filter(|handle| !handle.is_empty()) {
                    message.sender.name = format!("{} via @{handle}", message.sender.name);
                }
            }
            let mut wants = Vec::new();
            match &raw.media {
                None | Some(MessageMedia::Empty) => {}
                // `ttl_seconds` is view-once (`0x7FFFFFFF`) or a shorter timer. Either way the
                // bytes are not queued: the row below is a locked placeholder, opened in
                // Telegram's own app, and Save has nothing to copy.
                Some(MessageMedia::Photo(media)) if media.photo.is_some() && media.ttl_seconds.is_none() => {
                    if let Some(tl::enums::Photo::Photo(picture)) = &media.photo {
                        message.attachments.extend(photo(picture, media.spoiler, &mut wants));
                    }
                }
                Some(MessageMedia::Document(media)) if media.document.is_some() && media.ttl_seconds.is_none() => {
                    if let Some(tl::enums::Document::Document(file)) = &media.document {
                        message.attachments.push(document(media, file, &mut wants));
                    }
                }
                Some(MessageMedia::WebPage(media)) => {
                    if let tl::enums::WebPage::Page(page) = &media.webpage {
                        // The card is Telegram's own. Its picture is the row-sized still, fetched
                        // the way any thumbnail is. Instant View and the send controls stay outside
                        // this mapping.
                        message.previews.push(Preview {
                            url: page.url.clone(),
                            site: page.site_name.clone().unwrap_or_default(),
                            title: page.title.clone().unwrap_or_default(),
                            description: page.description.clone().unwrap_or_default(),
                            image: preview_image(page, &mut wants),
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
                // The title, then each item on its own list line. A done item is checked.
                Some(MessageMedia::ToDo(media)) => {
                    let link = message_link(peer, dir.get(peer), raw.id);
                    let (text, spans) = checklist(media, &link);
                    message.text = text;
                    message.spans = spans;
                }
                Some(MessageMedia::Contact(contact)) => {
                    message.contacts.push(shared_contact(contact));
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
                    // An invoice or paid preview is still a hand-off, and also a card: title,
                    // description and price. Paying stays in Telegram's own app. A shared place
                    // is a normal message whose card keeps the coordinates. No map tile is fetched.
                    if let Some(card) = detail_card(media).or_else(|| location_card(media, raw.date)) {
                        message.card = Some(Box::new(card));
                    } else if let MessageMedia::Poll(media) = media {
                        let tl::enums::Poll::Poll(poll) = &media.poll;
                        let tl::enums::PollResults::Results(results) = &media.results;
                        message.card = Some(Box::new(poll_card(poll, results)));
                    }
                }
            }
            let (topic, reply) = topic_of(raw.reply_to.as_ref());
            let discussion_top = match &raw.reply_to {
                Some(tl::enums::MessageReplyHeader::Header(header)) if !header.forum_topic => header.reply_to_top_id,
                _ => None,
            };
            let reply_note = if replies { None } else { reply_note(dir, peer, raw.reply_to.as_ref()) };
            Some(Mapped {
                peer,
                id: raw.id,
                topic,
                discussion_top,
                reply: if replies { None } else { reply },
                reply_note,
                message,
                wants,
                group: raw.grouped_id.filter(|id| *id != 0),
                members: Vec::new(),
            })
        }
    }
}

fn user_id(who: Who) -> String {
    match who {
        Who::User(_) => who.text(),
        _ => String::new(),
    }
}

fn reply_author(dir: &Directory, header: &tl::enums::MessageFwdHeader) -> (String, String) {
    let tl::enums::MessageFwdHeader::Header(header) = header;
    let id = header.from_id.as_ref().map(Who::of).map(user_id).unwrap_or_default();
    if let Some(name) = header.from_name.as_ref().filter(|name| !name.is_empty()) {
        return (name.clone(), id);
    }
    if let Some(who) = header.from_id.as_ref().map(Who::of) {
        let name = dir.name(who);
        if name != "Unknown" {
            return (name, id);
        }
        if let Some(author) = header.post_author.as_ref().filter(|name| !name.is_empty()) {
            return (author.clone(), id);
        }
        return (String::new(), id);
    }
    if let Some(author) = header.post_author.as_ref().filter(|name| !name.is_empty()) {
        return (author.clone(), String::new());
    }
    (String::new(), String::new())
}

/// The quote, the other chat, or the story a reply header names. `None` when the line is
/// just the answered message in this chat.
fn reply_note(dir: &Directory, peer: Who, header: Option<&tl::enums::MessageReplyHeader>) -> Option<ReplyNote> {
    match header? {
        tl::enums::MessageReplyHeader::Header(header) => {
            let quote = header.quote_text.clone().filter(|text| text.chars().any(|c| !c.is_whitespace())).unwrap_or_default();
            let other = header.reply_to_peer_id.as_ref().map(Who::of).filter(|who| *who != peer);
            let (author, author_id) = header.reply_from.as_ref().map(|from| reply_author(dir, from)).unwrap_or_default();
            let place = other
                .map(|who| {
                    let name = dir.name(who);
                    if name == "Unknown" { String::new() } else { name }
                })
                .unwrap_or_default();
            let elsewhere = other.is_some();
            if quote.is_empty() && author.is_empty() && !elsewhere {
                return None;
            }
            Some(ReplyNote { quote, author, author_id, elsewhere, place, story: false, story_id: 0 })
        }
        tl::enums::MessageReplyHeader::MessageReplyStoryHeader(story) => {
            let who = Who::of(&story.peer);
            let name = dir.name(who);
            let author = if name == "Unknown" { String::new() } else { name };
            Some(ReplyNote {
                quote: String::new(),
                author,
                author_id: user_id(who),
                elsewhere: who != peer,
                place: String::new(),
                story: true,
                story_id: story.story_id,
            })
        }
    }
}

/// The reply line. `known` is the answered message in this chat: its sender id, name, and text.
/// A partial quote replaces the start of that text. A reply in another chat does not use it.
pub(super) fn line_for(reply: i32, known: Option<&(String, String, String)>, note: Option<&ReplyNote>) -> ReplyTo {
    let elsewhere = note.is_some_and(|note| note.elsewhere);
    let story_id = note.and_then(|note| note.story.then_some(note.story_id));
    let id = story_id.map(|id| id.to_string()).unwrap_or_else(|| reply.to_string());
    let mut line = match known.filter(|_| !elsewhere && story_id.is_none()) {
        Some((_, sender, text)) => ReplyTo {
            id,
            sender: sender.clone(),
            snippet: text.chars().take(120).collect(),
            person: None,
            deleted: false,
        },
        None => ReplyTo { id, sender: String::new(), snippet: String::new(), person: None, deleted: false },
    };
    let Some(note) = note else {
        return line;
    };
    if note.story {
        line.snippet = "Story".to_owned();
        if !note.author.is_empty() {
            line.sender = note.author.clone();
        }
        return line;
    }
    if !note.quote.is_empty() {
        line.snippet = note.quote.chars().take(120).collect();
    } else if note.elsewhere {
        line.snippet = if note.place.is_empty() { "Another chat".to_owned() } else { format!("In {}", note.place) };
    }
    if (note.elsewhere || line.sender.is_empty()) && !note.author.is_empty() {
        line.sender = note.author.clone();
    }
    line
}

/// The lab's telegram-checklist check. Three items, the middle one done.
pub(super) fn prove_checklist() -> Result<(), String> {
    let raw = sample_checklist()?;
    let mapped = map(&Directory::new(7_001), &raw).ok_or("the checklist maps")?;
    if !mapped.wants.is_empty() {
        return Err("a checklist fetches nothing".into());
    }
    if mapped.message.kind != MessageKind::Normal {
        return Err("a checklist is the list, not a box that opens Telegram".into());
    }
    let text = "Packing\n\u{2610} Passport\n\u{2611} Tickets\n\u{2610} Charger\nOpen in Telegram";
    if mapped.message.text != text {
        return Err(format!("a three-item list with one done reads as the items, got {}", mapped.message.text));
    }
    let item = |line: &str| {
        let start = text.find(line).ok_or("the item is in the text")? as u32;
        Ok::<Span, &str>(Span { start, end: start + line.len() as u32, style: Style::ListItem { ordered: false, depth: 0 } })
    };
    let open = text.rfind("Open in Telegram").ok_or("the open link is in the text")? as u32;
    let mut expected = vec![
        item("\u{2610} Passport").map_err(str::to_owned)?,
        item("\u{2611} Tickets").map_err(str::to_owned)?,
        item("\u{2610} Charger").map_err(str::to_owned)?,
    ];
    expected.push(Span {
        start: open,
        end: text.len() as u32,
        style: Style::Link { url: "tg://openmessage?user_id=7002&message_id=7".into() },
    });
    if mapped.message.spans != expected {
        return Err("each item is its own list line, the done one is checked, and the message link is on the list".into());
    }
    Ok(())
}

fn sample_checklist() -> Result<tl::enums::Message, String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let words = |text: &str| tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() }.into();
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    7i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.from_id = Some(Who::User(7_002).peer());
    let item = |id: i32, title: &str| tl::types::TodoItem { id, title: words(title) }.into();
    let list = tl::types::TodoList {
        others_can_append: false,
        others_can_complete: true,
        title: words("Packing"),
        list: vec![item(1, "Passport"), item(2, "Tickets"), item(3, "Charger")],
    };
    let done = tl::types::TodoCompletion { id: 2, completed_by: Who::User(7_002).peer(), date: 1_700_000_100 };
    message.media = Some(tl::types::MessageMediaToDo { todo: list.into(), completions: Some(vec![done.into()]) }.into());
    Ok(message.into())
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

/// Telegram's album id, when this message is one item of a group.
pub fn grouped_id(raw: &tl::enums::Message) -> Option<i64> {
    match raw {
        tl::enums::Message::Message(message) => message.grouped_id.filter(|id| *id != 0),
        _ => None,
    }
}

fn merged_reactions(parts: &[&Mapped]) -> Vec<Reaction> {
    let mut out = Vec::new();
    for part in parts {
        for reaction in &part.message.reactions {
            let same = |have: &Reaction| {
                have.emoji == reaction.emoji && have.file.as_ref().map(|file| file.id.as_str()) == reaction.file.as_ref().map(|file| file.id.as_str())
            };
            if let Some(have) = out.iter_mut().find(|have| same(have)) {
                have.count = have.count.saturating_add(reaction.count);
                have.mine |= reaction.mine;
            } else {
                out.push(reaction.clone());
            }
        }
    }
    out
}

/// One album from its items. The row's id is the lowest Telegram id. The caption is the first
/// item that has words. Pictures stay in the order Telegram sent them.
pub fn merge_album(parts: &[Mapped]) -> Mapped {
    let mut ordered: Vec<&Mapped> = parts.iter().collect();
    ordered.sort_by_key(|part| part.id);
    let primary = ordered[0];
    let caption = ordered.iter().copied().find(|part| !part.message.text.is_empty()).unwrap_or(primary);
    let mut merged = caption.clone();
    let members: Vec<i32> = ordered.iter().map(|part| part.id).collect();
    let member_ids: HashSet<i32> = members.iter().copied().collect();
    merged.peer = primary.peer;
    merged.id = primary.id;
    merged.topic = primary.topic;
    merged.group = primary.group.or(caption.group);
    merged.discussion_top = ordered.iter().find_map(|part| part.discussion_top);
    merged.members = members;
    merged.message.id = primary.id.to_string();
    merged.message.sent = ordered.iter().map(|part| part.message.sent).max().unwrap_or(merged.message.sent);
    merged.message.attachments = ordered.iter().flat_map(|part| part.message.attachments.clone()).collect();
    merged.wants = ordered.iter().flat_map(|part| part.wants.clone()).collect();
    merged.message.reactions = merged_reactions(&ordered);
    merged.message.edited = ordered.iter().any(|part| part.message.edited);
    merged.message.edited_at = ordered.iter().filter_map(|part| part.message.edited_at).max();
    merged.message.pinned = ordered.iter().any(|part| part.message.pinned);
    merged.message.mentions_me = ordered.iter().any(|part| part.message.mentions_me);
    if merged.message.client_id.is_none() {
        merged.message.client_id = ordered.iter().find_map(|part| part.message.client_id.clone());
    }
    // A reply that names another item of this same album is not a reply to a different message.
    if merged.reply.is_some_and(|reply| member_ids.contains(&reply)) {
        merged.reply = None;
        merged.reply_note = None;
        merged.message.reply_to = None;
    }
    if merged.reply.is_none() {
        if let Some(part) = ordered.iter().find(|part| part.reply.is_some_and(|reply| !member_ids.contains(&reply))) {
            merged.reply = part.reply;
            merged.reply_note = part.reply_note.clone();
            merged.message.reply_to = part.message.reply_to.clone();
        }
    }
    merged
}

/// Newest first, as a page arrives. An album takes the seat of its newest item. Items that
/// share no group stay where they were.
pub fn group_albums(items: Vec<Mapped>) -> Vec<Mapped> {
    let mut emitted = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in &items {
        let Some(group) = item.group else {
            out.push(item.clone());
            continue;
        };
        if !emitted.insert((item.peer, group)) {
            continue;
        }
        let parts: Vec<Mapped> = items.iter().filter(|one| one.peer == item.peer && one.group == Some(group)).cloned().collect();
        out.push(merge_album(&parts));
    }
    out
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

fn album_item(id: i32, text: &str, group: i64, photo_id: i64) -> tl::enums::Message {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    id.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    text.to_owned().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
    message.from_id = Some(Who::User(7_002).peer());
    message.grouped_id = Some(group);
    message.media = Some(tl::types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(tl::types::Photo {
            has_stickers: false,
            id: photo_id,
            access_hash: 1,
            file_reference: vec![1],
            date: 1_700_000_000,
            sizes: vec![tl::types::PhotoSize { r#type: "y".to_owned(), w: 800, h: 600, size: 10 }.into()],
            video_sizes: None,
            dc_id: 2,
        }.into()),
        ttl_seconds: None,
        video: None,
    }.into());
    message.into()
}

/// The lab's telegram-stickers check. A Lottie sticker and a WebM sticker keep the still
/// and name the document, which is fetched with the row.
pub(super) fn prove_stickers() -> Result<(), String> {
    let one = |id: i64, mime: &str, kind: MediaKind| -> Result<(), String> {
        let raw = tl::types::Document {
            id,
            access_hash: id + 1,
            file_reference: vec![8],
            date: 1_700_000_000,
            mime_type: mime.to_owned(),
            size: 30_000,
            thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".into(), w: 128, h: 128, size: 4_000 }.into()]),
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::types::DocumentAttributeSticker {
                mask: false,
                alt: "🥾".into(),
                stickerset: tl::types::InputStickerSetId { id: 900, access_hash: 901 }.into(),
                mask_coords: None,
            }
            .into()],
        };
        let media = tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: None,
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        };
        let mut wants = Vec::new();
        let attachment = document(&media, &raw, &mut wants);
        let file_id = format!("d{id}");
        let thumb_id = format!("d{id}m");
        if !attachment.sticker || attachment.file.id != file_id || attachment.file.kind != kind || attachment.file.bytes != 30_000 {
            return Err(format!("{mime} sticker file was {} {:?}", attachment.file.id, attachment.file.kind));
        }
        if attachment.thumb.as_ref().is_none_or(|thumb| thumb.id != thumb_id) {
            return Err(format!("{mime} sticker dropped its still"));
        }
        if !wants.iter().any(|want| want.file.id == file_id && !want.original) {
            return Err(format!("{mime} sticker was not fetched with the row"));
        }
        Ok(())
    };
    one(6_410, "application/x-tgsticker", MediaKind::Emoji)?;
    one(6_411, "video/webm", MediaKind::Video)?;
    let (media, raw) = {
        let raw = tl::types::Document {
            id: 6_412,
            access_hash: 6_413,
            file_reference: vec![8],
            date: 1_700_000_000,
            mime_type: "image/webp".into(),
            size: 8_000,
            thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".into(), w: 128, h: 128, size: 4_000 }.into()]),
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::types::DocumentAttributeSticker {
                mask: false,
                alt: "🥾".into(),
                stickerset: tl::types::InputStickerSetId { id: 900, access_hash: 901 }.into(),
                mask_coords: None,
            }
            .into()],
        };
        let media = tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: None,
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        };
        (media, raw)
    };
    let mut wants = Vec::new();
    let still = document(&media, &raw, &mut wants);
    if still.file.id != "d6412m" || wants.iter().any(|want| want.file.id == "d6412") {
        return Err("a static sticker must stay its thumbnail".into());
    }
    Ok(())
}

/// The lab's telegram-link-preview check. A link card names its row-sized picture and asks for
/// that still, not the full photo.
pub(super) fn prove_link_preview() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    8_801i64.serialize(&mut bytes);
    "https://example.org/ridge".to_owned().serialize(&mut bytes);
    "example.org".to_owned().serialize(&mut bytes);
    1i32.serialize(&mut bytes);
    let mut page = tl::types::WebPage::from_bytes(&bytes).map_err(|_| "a blank page reads back".to_owned())?;
    page.title = Some("Ridge".to_owned());
    page.photo = Some(tl::types::Photo {
        has_stickers: false,
        id: 8_801,
        access_hash: 1,
        file_reference: vec![1],
        date: 1_700_000_000,
        sizes: vec![
            tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 240, size: 4_000 }.into(),
            tl::types::PhotoSize { r#type: "y".to_owned(), w: 1_280, h: 960, size: 200_000 }.into(),
        ],
        video_sizes: None,
        dc_id: 4,
    }.into());
    let mut message_bytes = Vec::new();
    0u32.serialize(&mut message_bytes);
    0u32.serialize(&mut message_bytes);
    32i32.serialize(&mut message_bytes);
    Who::User(7_002).peer().serialize(&mut message_bytes);
    1_700_000_000i32.serialize(&mut message_bytes);
    "https://example.org/ridge".to_owned().serialize(&mut message_bytes);
    let mut message = tl::types::Message::from_bytes(&message_bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.from_id = Some(Who::User(7_002).peer());
    message.media = Some(tl::types::MessageMediaWebPage {
        force_large_media: false,
        force_small_media: false,
        manual: false,
        safe: true,
        webpage: page.into(),
    }.into());
    let mapped = map(&Directory::new(7_001), &message.into()).ok_or("a link maps")?;
    let image = mapped.message.previews.first().and_then(|card| card.image.as_ref()).ok_or("a link card names no picture")?;
    if image.id != "p8801m" || image.width != 320 {
        return Err(format!("a link card's picture is the row-sized still, got {} at {}", image.id, image.width));
    }
    if !mapped.wants.iter().any(|want| want.file.id == "p8801m" && !want.original) || mapped.wants.iter().any(|want| want.original) {
        return Err("a link card fetches its still and not the full picture".into());
    }
    Ok(())
}

/// The lab's telegram-album check. Four items that share a grouped id are one message.
pub(super) fn prove_album() -> Result<(), String> {
    let dir = Directory::new(7_001);
    let raws: Vec<tl::enums::Message> = (0..4).map(|at| album_item(40 + at, if at == 0 { "Ridge in four" } else { "" }, 900, 6_040 + i64::from(at))).collect();
    let mapped: Vec<Mapped> = raws.iter().filter_map(|raw| map(&dir, raw)).collect();
    if mapped.len() != 4 {
        return Err(format!("four album items map, got {}", mapped.len()));
    }
    let mut newest_first = mapped.clone();
    newest_first.reverse();
    let grouped = group_albums(newest_first);
    let one = grouped.first().ok_or("an album groups")?;
    if grouped.len() != 1 || one.id != 40 || one.message.text != "Ridge in four" || one.message.attachments.len() != 4 {
        return Err(format!(
            "an album is one grid, got {} messages, id {}, text {}, {} attachments",
            grouped.len(),
            one.id,
            one.message.text,
            one.message.attachments.len()
        ));
    }
    let mut grown = Vec::new();
    for part in &mapped {
        grown.push(part.clone());
        let merged = merge_album(&grown);
        if merged.message.attachments.len() != grown.len() || merged.id != 40 || merged.message.text != "Ridge in four" {
            return Err("an album grows into one grid as its photos arrive".to_owned());
        }
    }
    Ok(())
}

/// The lab's telegram-quote check. A partial quote is the reply line, and a reply in another
/// chat shows that quote rather than a message in this chat that happens to share the id.
pub(super) fn prove_quote() -> Result<(), String> {
    let dir = Directory::new(1);
    let whole = "Meet at the gate and bring the rope tomorrow, then wait";
    let known = ("u7002".to_owned(), "Bea".to_owned(), whole.to_owned());
    let partial = map(&dir, &quote_message(Some("bring the rope"), None, None)).ok_or("a reply maps")?;
    let line = line_for(4, Some(&known), partial.reply_note.as_ref());
    if line.sender != "Bea" || line.snippet != "bring the rope" {
        return Err(format!("a partial quote did not replace the message, got {} / {}", line.sender, line.snippet));
    }
    let cross = map(&dir, &quote_message(Some("only this part"), Some(Who::Channel(90).peer()), Some("Nia"))).ok_or("a cross-chat reply maps")?;
    let line = line_for(4, Some(&known), cross.reply_note.as_ref());
    if line.sender != "Nia" || line.snippet != "only this part" || line.snippet.contains("Meet at the gate") {
        return Err(format!("a reply in another chat did not keep its quote, got {} / {}", line.sender, line.snippet));
    }
    let story = tl::types::MessageReplyStoryHeader { peer: Who::User(7_004).peer(), story_id: 15 }.into();
    let told = map(&dir, &quote_message_header(story)).ok_or("a story reply maps")?;
    let line = line_for(0, None, told.reply_note.as_ref());
    if line.id != "15" || line.snippet != "Story" {
        return Err(format!("a story reply was not named, got {} / {}", line.id, line.snippet));
    }
    Ok(())
}

fn quote_message(text: Option<&str>, peer: Option<tl::enums::Peer>, from_name: Option<&str>) -> tl::enums::Message {
    let reply_from = from_name.map(|name| {
        tl::types::MessageFwdHeader {
            imported: false,
            saved_out: false,
            from_id: None,
            from_name: Some(name.to_owned()),
            date: 1_700_000_000,
            channel_post: None,
            post_author: None,
            saved_from_peer: None,
            saved_from_msg_id: None,
            saved_from_id: None,
            saved_from_name: None,
            saved_date: None,
            psa_type: None,
        }
        .into()
    });
    let header = tl::types::MessageReplyHeader {
        reply_to_scheduled: false,
        forum_topic: false,
        quote: text.is_some(),
        reply_to_ephemeral: false,
        reply_to_msg_id: Some(4),
        reply_to_peer_id: peer,
        reply_from,
        reply_media: None,
        reply_to_top_id: None,
        quote_text: text.map(str::to_owned),
        quote_entities: None,
        quote_offset: Some(9),
        todo_item_id: None,
        poll_option: None,
    };
    quote_message_header(header.into())
}

fn quote_message_header(reply_to: tl::enums::MessageReplyHeader) -> tl::enums::Message {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    9i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
    message.message = "On my way".to_owned();
    message.reply_to = Some(reply_to);
    message.from_id = Some(Who::User(7_003).peer());
    message.into()
}

/// The lab's telegram-polls check. A poll is a card with its options and counts, a quiz hides
/// those counts until the poll closes, and both stay a hand-off.
pub(super) fn prove_polls() -> Result<(), String> {
    let words = |text: &str| tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() });
    let answer = |text: &str, option: u8| {
        tl::types::PollAnswer { text: words(text), option: vec![option], media: None, added_by: None, date: None }.into()
    };
    let row = |option: u8, count: i32, chosen: bool, correct: bool| {
        tl::types::PollAnswerVoters { chosen, correct, option: vec![option], voters: Some(count), recent_voters: None }.into()
    };
    let poll = |hide: bool, quiz: bool| tl::types::Poll {
        id: 1,
        closed: false,
        public_voters: false,
        multiple_choice: false,
        quiz,
        open_answers: false,
        revoting_disabled: false,
        shuffle_answers: false,
        hide_results_until_close: hide,
        creator: false,
        subscribers_only: false,
        question: words("Saturday or Sunday?"),
        answers: vec![answer("Saturday", 0), answer("Sunday", 1)],
        close_period: None,
        close_date: None,
        countries_iso2: None,
        hash: 0,
    };
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    21i32.serialize(&mut bytes);
    Who::Group(8_001).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut raw = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    raw.from_id = Some(Who::User(7_003).peer());
    let results = tl::types::PollResults {
        min: false,
        has_unread_votes: false,
        can_view_stats: false,
        results: Some(vec![row(0, 3, true, false), row(1, 1, false, false)]),
        total_voters: Some(4),
        recent_voters: None,
        solution: None,
        solution_entities: None,
        solution_media: None,
    };
    raw.media = Some(tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll {
        poll: poll(false, false).into(),
        results: results.into(),
        attached_media: None,
    })));
    let mapped = map(&Directory::new(7_001), &raw.clone().into()).ok_or("the poll maps")?;
    let card = mapped.message.card.as_ref().ok_or("a poll is a card")?;
    if card.title != "Saturday or Sunday?" || card.footer != "Open in Telegram" || card.text != "4 votes" {
        return Err(format!("a poll's card lost its question or its count: {} / {}", card.title, card.text));
    }
    if card.fields.first().map(|(label, value)| (label.as_str(), value.as_str())) != Some(("3 votes · your vote", "Saturday")) {
        return Err(format!("the chosen option was not named: {:?}", card.fields));
    }
    if !matches!(mapped.message.kind, MessageKind::Unsupported { ref link } if link.contains("message_id=21")) {
        return Err("a poll is still a hand-off".into());
    }
    let hidden = tl::types::PollResults {
        min: true,
        has_unread_votes: false,
        can_view_stats: false,
        results: Some(vec![row(0, 9, false, true)]),
        total_voters: Some(9),
        recent_voters: None,
        solution: Some("The north ridge.".into()),
        solution_entities: None,
        solution_media: None,
    };
    raw.media = Some(tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll {
        poll: poll(true, true).into(),
        results: hidden.into(),
        attached_media: None,
    })));
    let quiz = map(&Directory::new(7_001), &raw.into()).ok_or("the quiz maps")?;
    let card = quiz.message.card.as_ref().ok_or("a quiz is a card")?;
    if !card.text.contains("hidden") || !card.text.contains("The north ridge.") || card.fields.iter().any(|(label, _)| label.contains('9')) {
        return Err(format!("a hidden quiz showed a count: {}", card.text));
    }
    Ok(())
}

/// The lab's telegram-edited check. An edit arrives with its time, and the store keeps what
/// the words were before, where the replace would otherwise forget both.
pub(super) fn prove_edited() -> Result<(), String> {
    use crate::store::{ConversationRow, Store};
    let dir = Directory::new(1);
    let first = map(&dir, &edited_message("On my way", None)).ok_or("a plain message maps")?;
    if first.message.edited || first.message.edited_at.is_some() {
        return Err("a message nobody edited says it was".into());
    }
    let second = map(&dir, &edited_message("On my way, the rope too", Some(1_700_000_100)))
        .ok_or("an edited message maps")?;
    if !second.message.edited || second.message.edited_at != Some(1_700_000_100) {
        return Err("an edit did not keep its fact or its time".into());
    }
    let mut store = Store::open(std::path::Path::new(":memory:")).map_err(|err| format!("a store: {err}"))?;
    let account = protocol::AccountKey { network: protocol::Network::Telegram, account: "me".to_owned() };
    let row = ConversationRow::new("chat", protocol::ConversationKind::Direct, "Ada");
    store.upsert_conversation(&account, &row).map_err(|err| format!("a conversation: {err}"))?;
    let key = protocol::Key::new(&account, "chat");
    store.ingest(&key, &first.message, false).map_err(|err| format!("the message: {err}"))?;
    store.ingest(&key, &second.message, false).map_err(|err| format!("the edit: {err}"))?;
    let page = store
        .messages(&key, &protocol::Page::Newest, 10, 1_700_000_200)
        .map_err(|err| format!("a page: {err}"))?
        .ok_or("the conversation holds the edit")?;
    let kept = page.messages.first().ok_or("the edited row")?;
    if !kept.edited || kept.edited_at != Some(1_700_000_100) || kept.prior_text.as_deref() != Some("On my way") {
        return Err(format!(
            "the row kept the edit wrong: edited {}, at {:?}, prior {:?}",
            kept.edited,
            kept.edited_at,
            kept.prior_text.as_deref()
        ));
    }
    Ok(())
}

/// A plain Telegram message from Ada, edited at `edit_date` when it is given.
fn edited_message(text: &str, edit_date: Option<i32>) -> tl::enums::Message {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    9i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    text.to_owned().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
    message.from_id = Some(Who::User(7_002).peer());
    message.edit_date = edit_date;
    message.into()
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

/// The lab's telegram-invoices check. An invoice and a paid preview are cards with a price,
/// and a payment names its amount. A build from before this has no card.
pub(super) fn prove_invoices() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    9i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.media = Some(
        tl::types::MessageMediaInvoice {
            shipping_address_requested: false,
            test: false,
            title: "Ridge map".into(),
            description: "The weekend route".into(),
            photo: None,
            receipt_msg_id: None,
            currency: "USD".into(),
            total_amount: 500,
            start_param: String::new(),
            extended_media: None,
        }
        .into(),
    );
    let mapped = map(&Directory::new(7_001), &message.into()).ok_or("the invoice maps")?;
    let card = mapped.message.card.ok_or("an invoice is a card")?;
    if card.title != "Ridge map" || card.fields.first().map(|field| field.1.as_str()) != Some("USD 5.00") {
        return Err(format!("the invoice card has no price, got {}", card.title));
    }
    match mapped.message.kind {
        MessageKind::Unsupported { ref link } if link.contains("message_id=9") => {}
        _ => return Err("an invoice does not open in Telegram".into()),
    }
    let mut paid = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    paid.id = 10;
    paid.media = Some(
        tl::types::MessageMediaPaidMedia {
            stars_amount: 50,
            extended_media: vec![tl::types::MessageExtendedMediaPreview { w: Some(320), h: Some(240), thumb: None, video_duration: None }.into()],
        }
        .into(),
    );
    let mapped = map(&Directory::new(7_001), &paid.into()).ok_or("the paid media maps")?;
    let card = mapped.message.card.ok_or("paid media is a card")?;
    if card.fields.first().map(|field| field.1.as_str()) != Some("50 Stars") || !card.text.to_lowercase().contains("blur") {
        return Err(format!("the paid preview is unnamed, got {}", card.text));
    }
    let sent = tl::types::MessageActionPaymentSent {
        recurring_init: false,
        recurring_used: false,
        currency: "XTR".into(),
        total_amount: 50,
        invoice_slug: None,
        subscription_until_date: None,
    };
    let (line, known) = action(&Directory::new(1), "Ada", &sent.into());
    if !known || line != "Ada sent a payment of 50 Stars" {
        return Err(format!("a Star payment is unnamed, got {line}"));
    }
    Ok(())
}

/// The lab's telegram-location check. A point and a venue are cards with coordinates, and a
/// live location keeps its place. A build from before this names them and drops the numbers.
pub(super) fn prove_location() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    9i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let point = tl::types::GeoPoint { long: -122.4194, lat: 37.7749, access_hash: 0, accuracy_radius: None };
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.media = Some(tl::types::MessageMediaGeo { geo: point.clone().into() }.into());
    let mapped = map(&Directory::new(7_001), &message.into()).ok_or("the location maps")?;
    let card = mapped.message.card.ok_or("a location is a card")?;
    if card.title != "Location" || card.footer != "Open in Maps" || !card.text.contains("geo:37.7749,-122.4194") {
        return Err(format!("the location card dropped its place, got {} {}", card.title, card.text));
    }
    if !matches!(mapped.message.kind, MessageKind::Normal) {
        return Err("a location still opens only in Telegram".into());
    }
    let mut venue = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    venue.id = 10;
    venue.media = Some(
        tl::types::MessageMediaVenue {
            geo: point.into(),
            title: "Ferry Building".into(),
            address: "1 The Embarcadero".into(),
            provider: String::new(),
            venue_id: String::new(),
            venue_type: String::new(),
        }
        .into(),
    );
    let mapped = map(&Directory::new(7_001), &venue.into()).ok_or("the venue maps")?;
    let card = mapped.message.card.ok_or("a venue is a card")?;
    if card.title != "Ferry Building" || card.text != "1 The Embarcadero" {
        return Err(format!("the venue card dropped its name, got {} {}", card.title, card.text));
    }
    let mut live = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    live.id = 11;
    live.media = Some(
        tl::types::MessageMediaGeoLive {
            geo: tl::types::GeoPoint { long: -122.4, lat: 37.7, access_hash: 0, accuracy_radius: None }.into(),
            heading: Some(90),
            period: 900,
            proximity_notification_radius: None,
        }
        .into(),
    );
    let mapped = map(&Directory::new(7_001), &live.into()).ok_or("the live location maps")?;
    let card = mapped.message.card.ok_or("a live location is a card")?;
    if card.title != "Live location" || !card.text.contains("geo:37.7,-122.4") {
        return Err(format!("the live location dropped its place, got {} {}", card.title, card.text));
    }
    Ok(())
}

/// The lab's telegram-view-once check. A view-once photo and a listen-once voice message
/// queue no file and read as locked. An ordinary photo is still fetched.
pub(super) fn prove_view_once() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};

    fn blank(id: i32) -> tl::types::Message {
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        id.serialize(&mut bytes);
        Who::User(7_002).peer().serialize(&mut bytes);
        1_700_000_000i32.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
        message.from_id = Some(Who::User(7_002).peer());
        message
    }

    let photo = |ttl: Option<i32>| {
        let mut message = blank(14);
        message.media = Some(
            tl::types::MessageMediaPhoto {
                spoiler: false,
                live_photo: false,
                photo: Some(
                    tl::types::Photo {
                        has_stickers: false,
                        id: 6_014,
                        access_hash: 6_015,
                        file_reference: vec![1],
                        date: 1_700_000_000,
                        sizes: vec![tl::types::PhotoSize { r#type: "m".into(), w: 320, h: 240, size: 4_000 }.into()],
                        video_sizes: None,
                        dc_id: 4,
                    }
                    .into(),
                ),
                ttl_seconds: ttl,
                video: None,
            }
            .into(),
        );
        tl::enums::Message::from(message)
    };
    let mapped = |raw: &tl::enums::Message| map(&Directory::new(7_001), raw).ok_or("the message maps");
    let ordinary = mapped(&photo(None))?;
    if ordinary.wants.is_empty() || ordinary.message.attachments.len() != 1 {
        return Err("an ordinary photo is still fetched".into());
    }
    let once = mapped(&photo(Some(i32::MAX)))?;
    if !once.wants.is_empty() || !once.message.attachments.is_empty() {
        return Err("a view-once photo queues no file".into());
    }
    if !once.message.text.contains("View-once photo") || !matches!(once.message.kind, MessageKind::Unsupported { .. }) {
        return Err(format!("a view-once photo reads as locked, got {}", once.message.text));
    }
    let timed = mapped(&photo(Some(30)))?;
    if !timed.wants.is_empty() || !timed.message.text.contains("Photo with a timer") {
        return Err(format!("a photo with a timer stays locked, got {}", timed.message.text));
    }
    let voice_doc = tl::types::Document {
        id: 6_100,
        access_hash: 6_101,
        file_reference: vec![4],
        date: 1_700_000_000,
        mime_type: "audio/ogg".into(),
        size: 18_000,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl::types::DocumentAttributeAudio { voice: true, duration: 7, title: None, performer: None, waveform: Some(vec![0b0001_1111]) }.into()],
    };
    let mut voice = blank(15);
    voice.media = Some(
        tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: true,
            document: Some(voice_doc.into()),
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: Some(i32::MAX),
        }
        .into(),
    );
    let voice = mapped(&voice.into())?;
    if !voice.wants.is_empty() || !voice.message.attachments.is_empty() || !voice.message.text.contains("View-once voice message") {
        return Err(format!("a listen-once voice message stays locked, got {}", voice.message.text));
    }
    Ok(())
}

/// The lab's telegram-inline-bots check. A message sent through a bot the account already
/// knows names that bot. A build from before this leaves the name as the person only.
pub(super) fn prove_inline_bots() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    7_060i64.serialize(&mut bytes);
    let mut user = tl::types::User::from_bytes(&bytes).map_err(|_| "a blank user reads back".to_owned())?;
    user.first_name = Some("Wiki".to_owned());
    user.username = Some("wiki".to_owned());
    user.access_hash = Some(1);
    user.bot = true;
    user.bot_info_version = Some(1);
    let mut dir = Directory::new(7_001);
    dir.learn(&[user.into()], &[]);
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    9i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    1_700_000_000i32.serialize(&mut bytes);
    "Rust".to_owned().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.from_id = Some(Who::User(7_002).peer());
    message.via_bot_id = Some(7_060);
    let mapped = map(&dir, &message.into()).ok_or("the message maps")?;
    if !mapped.message.sender.name.contains("via @wiki") {
        return Err(format!("an inline result did not name the bot, got {}", mapped.message.sender.name));
    }
    Ok(())
}

/// The lab's telegram-live-location check. A live share keeps its coordinates and says whether
/// it is still going, and coming into range is a service line. A build from before this drops
/// the coordinates.
pub(super) fn prove_live_location() -> Result<(), String> {
    use grammers_tl_types::{Deserializable, Serializable};
    let mut bytes = Vec::new();
    0u32.serialize(&mut bytes);
    0u32.serialize(&mut bytes);
    11i32.serialize(&mut bytes);
    Who::User(7_002).peer().serialize(&mut bytes);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i32;
    now.serialize(&mut bytes);
    String::new().serialize(&mut bytes);
    let mut message = tl::types::Message::from_bytes(&bytes).map_err(|_| "a blank message reads back".to_owned())?;
    message.from_id = Some(Who::User(7_002).peer());
    message.media = Some(
        tl::types::MessageMediaGeoLive {
            geo: tl::types::GeoPoint { long: -122.4, lat: 37.7, access_hash: 0, accuracy_radius: None }.into(),
            heading: Some(90),
            period: 900,
            proximity_notification_radius: None,
        }
        .into(),
    );
    let mapped = map(&Directory::new(7_001), &message.into()).ok_or("the live location maps")?;
    let card = mapped.message.card.ok_or("a live location is a card")?;
    if card.title != "Live location" || !card.text.contains("geo:37.7,-122.4") || !card.fields.iter().any(|field| field.0 == "Status" && field.1 == "Still sharing") {
        return Err(format!("the live location dropped its place, got {} {}", card.title, card.text));
    }
    if !matches!(mapped.message.kind, MessageKind::Normal) {
        return Err("a live location still opens only in Telegram".into());
    }
    let reached = tl::types::MessageActionGeoProximityReached {
        from_id: Who::User(7_002).peer(),
        to_id: Who::User(7_001).peer(),
        distance: 50,
    };
    let (line, known) = action(&Directory::new(7_001), "Ada", &reached.into());
    if !known || !line.contains("50 m") {
        return Err(format!("coming into range is unnamed, got {line}"));
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
    fn a_reaction_names_its_reactors_only_where_telegram_shows_the_list() {
        let emoji = |emoticon: &str| tl::enums::Reaction::Emoji(tl::types::ReactionEmoji { emoticon: emoticon.to_owned() }.into());
        let reacted = |user: i64, emoticon: &str| tl::types::MessagePeerReaction {
            big: false,
            unread: false,
            my: false,
            peer_id: tl::enums::Peer::User(tl::types::PeerUser { user_id: user }),
            date: 0,
            reaction: emoji(emoticon),
        };
        let results = |count: i32, emoticon: &str| tl::enums::ReactionCount::Count(tl::types::ReactionCount {
            chosen_order: None,
            reaction: emoji(emoticon),
            count,
        });
        let listed = |shown: &tl::types::MessageReactions| -> Vec<Vec<String>> {
            reactions(&shown.clone().into()).iter().map(|one| one.who.clone()).collect()
        };
        let mut shown = tl::types::MessageReactions {
            min: false,
            can_see_list: true,
            reactions_as_tags: false,
            results: vec![results(2, "👍"), results(1, "❤")],
            recent_reactions: Some(vec![reacted(42, "👍").into(), reacted(43, "👍").into(), reacted(44, "❤").into()]),
            top_reactors: None,
        };
        // `can_see_list` set: the list already sent with the message names each reactor.
        assert_eq!(listed(&shown), [vec!["u42".to_owned(), "u43".to_owned()], vec!["u44".to_owned()]]);
        // A channel or a large group hides the list: the chip stays a number.
        shown.can_see_list = false;
        assert!(listed(&shown).iter().all(|one| one.is_empty()));
        // A `min` message carries only part of the picture: nothing is named either.
        let partial = tl::types::MessageReactions { min: true, ..shown.clone() };
        assert!(listed(&partial).iter().all(|one| one.is_empty()));
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

    fn blank_page(url: &str) -> tl::types::WebPage {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        8_801i64.serialize(&mut bytes);
        url.to_owned().serialize(&mut bytes);
        "example.org".to_owned().serialize(&mut bytes);
        1i32.serialize(&mut bytes);
        tl::types::WebPage::from_bytes(&bytes).expect("a blank page reads back")
    }

    fn link_message(photo: bool, document: bool) -> tl::enums::Message {
        let mut message = blank_message();
        message.id = 32;
        let mut page = blank_page("https://example.org/ridge");
        page.site_name = Some("Example".to_owned());
        page.title = Some("Ridge".to_owned());
        page.description = Some("The trail.".to_owned());
        if photo {
            page.photo = Some(tl::types::Photo {
                has_stickers: false,
                id: 8_801,
                access_hash: 1,
                file_reference: vec![1],
                date: 1_700_000_000,
                sizes: vec![
                    tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 240, size: 4_000 }.into(),
                    tl::types::PhotoSize { r#type: "y".to_owned(), w: 1_280, h: 960, size: 200_000 }.into(),
                ],
                video_sizes: None,
                dc_id: 4,
            }.into());
        }
        if document {
            page.document = Some(tl::types::Document {
                id: 8_802,
                access_hash: 1,
                file_reference: vec![2],
                date: 1_700_000_000,
                mime_type: "video/mp4".to_owned(),
                size: 1_000,
                thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".to_owned(), w: 320, h: 180, size: 20 }.into()]),
                video_thumbs: None,
                dc_id: 2,
                attributes: Vec::new(),
            }.into());
        }
        message.media = Some(tl::types::MessageMediaWebPage {
            force_large_media: false,
            force_small_media: false,
            manual: false,
            safe: true,
            webpage: page.into(),
        }.into());
        message.into()
    }

    #[test]
    fn a_link_card_names_its_picture_and_asks_for_the_thumb_only() {
        let card = map(&Directory::new(7_001), &link_message(true, false)).expect("a link maps");
        let image = card.message.previews[0].image.as_ref().expect("the card names its picture");
        assert_eq!(image.id, "p8801m");
        assert!(card.wants.iter().any(|want| want.file.id == "p8801m" && !want.original));
        assert!(card.wants.iter().all(|want| !want.original));
        let video = map(&Directory::new(7_001), &link_message(false, true)).expect("a video card maps");
        assert_eq!(video.message.previews[0].image.as_ref().expect("a video card names its still").id, "d8802m");
        assert!(video.wants.iter().all(|want| want.file.id == "d8802m" && !want.original));
        let bare = map(&Directory::new(7_001), &link_message(false, false)).expect("a card without a picture maps");
        assert!(bare.message.previews[0].image.is_none());
        assert!(bare.wants.is_empty());
    }

    #[test]
    fn a_dice_roll_keeps_its_value() {
        let rolled = map(&Directory::new(7_001), &dice_message(5)).expect("the roll maps");
        assert!(rolled.wants.is_empty());
        assert_eq!(rolled.message.text, "\u{1F3B2} 5");
        let spinning = map(&Directory::new(7_001), &dice_message(0)).expect("a roll still spinning maps");
        assert_eq!(spinning.message.text, "\u{1F3B2}");
    }

    fn words(text: &str) -> tl::enums::TextWithEntities {
        tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities { text: text.to_owned(), entities: Vec::new() })
    }

    fn poll_answer(text: &str, option: u8) -> tl::enums::PollAnswer {
        tl::types::PollAnswer {
            text: words(text),
            option: vec![option],
            media: None,
            added_by: None,
            date: None,
        }
        .into()
    }

    fn poll_voters(option: u8, count: i32, chosen: bool, correct: bool) -> tl::enums::PollAnswerVoters {
        tl::types::PollAnswerVoters {
            chosen,
            correct,
            option: vec![option],
            voters: Some(count),
            recent_voters: None,
        }
        .into()
    }

    fn poll_message(poll: tl::types::Poll, results: tl::types::PollResults) -> tl::enums::Message {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        21i32.serialize(&mut bytes);
        Who::Group(8_001).peer().serialize(&mut bytes);
        1_700_000_000i32.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
        message.from_id = Some(Who::User(7_003).peer());
        message.media = Some(
            tl::enums::MessageMedia::Poll(Box::new(tl::types::MessageMediaPoll {
                poll: poll.into(),
                results: results.into(),
                attached_media: None,
            })),
        );
        message.into()
    }

    fn open_poll(hide: bool, quiz: bool, closed: bool) -> tl::types::Poll {
        tl::types::Poll {
            id: 1,
            closed,
            public_voters: false,
            multiple_choice: false,
            quiz,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: hide,
            creator: false,
            subscribers_only: false,
            question: words("Saturday or Sunday?"),
            answers: vec![poll_answer("Saturday", 0), poll_answer("Sunday", 1)],
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        }
    }

    /// Options, counts, and the vote already cast are on the card. Voting stays a hand-off.
    #[test]
    fn a_poll_is_a_card_with_its_options_and_a_hand_off() {
        let results = tl::types::PollResults {
            min: false,
            has_unread_votes: false,
            can_view_stats: false,
            results: Some(vec![poll_voters(0, 3, true, false), poll_voters(1, 1, false, false)]),
            total_voters: Some(4),
            recent_voters: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        };
        let mapped = map(&Directory::new(7_001), &poll_message(open_poll(false, false, false), results)).expect("the poll maps");
        assert_eq!(mapped.message.text, "Poll: Saturday or Sunday?");
        match &mapped.message.kind {
            MessageKind::Unsupported { link } => assert!(link.contains("message_id=21"), "{link}"),
            other => panic!("a poll stays a hand-off, got {other:?}"),
        }
        let card = mapped.message.card.as_ref().expect("a poll is a card");
        assert_eq!(card.title, "Saturday or Sunday?");
        assert_eq!(card.footer, "Open in Telegram");
        assert_eq!(card.text, "4 votes");
        assert_eq!(
            card.fields,
            vec![
                ("3 votes · your vote".to_owned(), "Saturday".to_owned()),
                ("1 vote".to_owned(), "Sunday".to_owned()),
            ]
        );

        let hidden = tl::types::PollResults {
            min: true,
            has_unread_votes: false,
            can_view_stats: false,
            results: Some(vec![poll_voters(0, 9, false, true), poll_voters(1, 9, true, false)]),
            total_voters: Some(9),
            recent_voters: None,
            solution: Some("The north ridge.".to_owned()),
            solution_entities: None,
            solution_media: None,
        };
        let quiz = map(&Directory::new(7_001), &poll_message(open_poll(true, true, false), hidden)).expect("the quiz maps");
        let card = quiz.message.card.as_ref().expect("a quiz is a card");
        assert_eq!(card.title, "Saturday or Sunday?");
        assert!(card.text.contains("hidden"), "{}", card.text);
        assert!(card.text.contains("Quiz"), "{}", card.text);
        assert!(card.text.contains("The north ridge."), "{}", card.text);
        assert!(card.fields.iter().all(|(_, value)| !value.chars().any(|ch| ch.is_ascii_digit())), "hidden counts stay off the card");
        assert!(card.fields.iter().any(|(label, value)| value == "Saturday" && label.to_lowercase().contains("correct")));
        assert!(card.fields.iter().any(|(label, value)| value == "Sunday" && label.to_lowercase().contains("your vote")));
    }

    fn said(action: MessageAction) -> String {
        super::action(&Directory::new(1), "Ada", &action).0
    }

    fn blank_message() -> tl::types::Message {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        9i32.serialize(&mut bytes);
        Who::User(7_002).peer().serialize(&mut bytes);
        1_700_000_000i32.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
        message.from_id = Some(Who::User(7_002).peer());
        message
    }

    fn point(lat: f64, long: f64) -> tl::enums::GeoPoint {
        tl::types::GeoPoint { long, lat, access_hash: 0, accuracy_radius: None }.into()
    }

    #[test]
    fn a_shared_location_and_a_venue_keep_their_place() {
        let mut place = blank_message();
        place.media = Some(tl::types::MessageMediaGeo { geo: point(37.7749, -122.4194) }.into());
        let mapped = map(&Directory::new(7_001), &place.into()).expect("the location maps");
        assert!(matches!(mapped.message.kind, MessageKind::Normal), "a location is the card, not a hand-off");
        let card = mapped.message.card.expect("a location is a card");
        assert_eq!(card.title, "Location");
        assert_eq!(card.footer, "Open in Maps");
        assert!(card.text.contains("geo:37.7749,-122.4194"), "{}", card.text);
        assert!(card.fields.iter().any(|(label, value)| label == "Latitude" && value.starts_with("37.7749")));
        assert!(card.fields.iter().any(|(label, value)| label == "Longitude" && value.starts_with("-122.4194")));

        let mut venue = blank_message();
        venue.id = 10;
        venue.media = Some(
            tl::types::MessageMediaVenue {
                geo: point(37.7955, -122.3937),
                title: "Ferry Building".into(),
                address: "1 The Embarcadero".into(),
                provider: String::new(),
                venue_id: String::new(),
                venue_type: String::new(),
            }
            .into(),
        );
        let mapped = map(&Directory::new(7_001), &venue.into()).expect("the venue maps");
        assert!(matches!(mapped.message.kind, MessageKind::Normal));
        let card = mapped.message.card.expect("a venue is a card");
        assert_eq!(card.title, "Ferry Building");
        assert_eq!(card.text, "1 The Embarcadero");
        assert_eq!(card.footer, "Open in Maps");
        assert!(card.fields.iter().any(|(label, value)| label == "Latitude" && value.starts_with("37.7955")));
    }

    fn bot(id: i64, name: &str, handle: &str) -> tl::types::User {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        id.serialize(&mut bytes);
        let mut user = tl::types::User::from_bytes(&bytes).expect("a blank user reads back");
        user.first_name = Some(name.to_owned());
        user.username = Some(handle.to_owned());
        user.access_hash = Some(1);
        user.bot = true;
        user.bot_info_version = Some(1);
        user
    }

    #[test]
    fn a_message_sent_through_an_inline_bot_names_that_bot() {
        let mut dir = Directory::new(7_001);
        dir.learn(&[bot(7_060, "Wiki", "wiki").into()], &[]);
        let mut message = blank_message();
        message.message = "Rust".into();
        message.via_bot_id = Some(7_060);
        let mapped = map(&dir, &message.into()).expect("the message maps");
        assert!(mapped.message.sender.name.contains("via @wiki"), "{}", mapped.message.sender.name);
        let plain = map(&dir, &blank_message().into()).expect("a plain message maps");
        assert!(!plain.message.sender.name.contains("via @"), "{}", plain.message.sender.name);
    }

    fn live(id: i32, date: i32, lat: f64, long: f64, period: i32) -> tl::enums::Message {
        let mut message = blank_message();
        message.id = id;
        message.date = date;
        message.media = Some(
            tl::types::MessageMediaGeoLive {
                geo: point(lat, long),
                heading: Some(90),
                period,
                proximity_notification_radius: None,
            }
            .into(),
        );
        message.into()
    }

    #[test]
    fn a_live_location_keeps_its_place_and_says_whether_it_is_still_shared() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i32;
        let sharing = map(&Directory::new(7_001), &live(11, now, 37.7, -122.4, 900)).expect("the live location maps");
        assert!(matches!(sharing.message.kind, MessageKind::Normal), "a live location is the card, not a hand-off");
        let card = sharing.message.card.expect("a live location is a card");
        assert_eq!(card.title, "Live location");
        assert_eq!(card.footer, "Open in Maps");
        assert!(card.text.contains("geo:37.7,-122.4"), "{}", card.text);
        assert!(card.fields.iter().any(|(label, value)| label == "Heading" && value == "90°"));
        assert!(card.fields.iter().any(|(label, value)| label == "Status" && value == "Still sharing"), "{:?}", card.fields);

        let ended = map(&Directory::new(7_001), &live(11, 1_700_000_000, 37.7, -122.4, 900)).expect("an ended live location maps");
        let card = ended.message.card.expect("an ended live location is a card");
        assert!(card.fields.iter().any(|(label, value)| label == "Status" && value == "Stopped"), "{:?}", card.fields);

        let moved = map(&Directory::new(7_001), &live(11, now, 37.8, -122.5, 900)).expect("a moved live location maps");
        let card = moved.message.card.expect("a moved live location is a card");
        assert!(card.text.contains("geo:37.8,-122.5"), "{}", card.text);
    }

    #[test]
    fn coming_within_range_of_a_live_location_is_a_service_line() {
        let dir = Directory::new(7_001);
        let reached = tl::types::MessageActionGeoProximityReached {
            from_id: Who::User(7_002).peer(),
            to_id: Who::User(7_001).peer(),
            distance: 50,
        };
        let (line, known) = super::action(&dir, "Ada", &reached.into());
        assert!(known, "{line}");
        assert!(line.contains("50 m"), "{line}");
        assert!(line.to_lowercase().contains("you"), "{line}");
    }

    #[test]
    fn an_invoice_and_paid_media_are_cards_with_a_price_and_a_hand_off() {
        let mut invoice = blank_message();
        invoice.media = Some(
            tl::types::MessageMediaInvoice {
                shipping_address_requested: false,
                test: false,
                title: "Ridge map".into(),
                description: "The weekend route".into(),
                photo: None,
                receipt_msg_id: None,
                currency: "USD".into(),
                total_amount: 500,
                start_param: String::new(),
                extended_media: None,
            }
            .into(),
        );
        let mapped = map(&Directory::new(7_001), &invoice.into()).expect("the invoice maps");
        let card = mapped.message.card.expect("an invoice is a card");
        assert_eq!(card.title, "Ridge map");
        assert_eq!(card.text, "The weekend route");
        assert_eq!(card.fields, vec![("Price".into(), "USD 5.00".into())]);
        assert!(matches!(mapped.message.kind, MessageKind::Unsupported { ref link } if link.contains("message_id=9")));

        let mut paid = blank_message();
        paid.id = 10;
        paid.media = Some(
            tl::types::MessageMediaPaidMedia {
                stars_amount: 50,
                extended_media: vec![tl::types::MessageExtendedMediaPreview {
                    w: Some(1_280),
                    h: Some(720),
                    thumb: None,
                    video_duration: Some(8),
                }
                .into()],
            }
            .into(),
        );
        let mapped = map(&Directory::new(7_001), &paid.into()).expect("the paid media maps");
        let card = mapped.message.card.expect("paid media is a card");
        assert_eq!(card.title, "Paid media");
        assert!(card.text.to_lowercase().contains("lock") || card.text.to_lowercase().contains("blur"), "{}", card.text);
        assert_eq!(card.fields, vec![("Price".into(), "50 Stars".into())]);
        assert!(matches!(mapped.message.kind, MessageKind::Unsupported { .. }));
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
        assert_eq!(said(paid.into()), "Ada sent a payment of USD 5.00");
        let stars = tl::types::MessageActionPaymentSent {
            recurring_init: false,
            recurring_used: false,
            currency: "XTR".into(),
            total_amount: 50,
            invoice_slug: None,
            subscription_until_date: None,
        };
        assert_eq!(said(stars.into()), "Ada sent a payment of 50 Stars");
        let refunded = tl::types::MessageActionPaymentRefunded {
            peer: Who::User(7_002).peer(),
            currency: "USD".into(),
            total_amount: 500,
            payload: None,
            charge: tl::types::PaymentCharge { id: "c".into(), provider_charge_id: "p".into() }.into(),
        };
        assert_eq!(said(refunded.into()), "A payment of USD 5.00 was refunded");
        let stars_back = tl::types::MessageActionPaymentRefunded {
            peer: Who::User(7_002).peer(),
            currency: "XTR".into(),
            total_amount: 1,
            payload: None,
            charge: tl::types::PaymentCharge { id: "c".into(), provider_charge_id: "p".into() }.into(),
        };
        assert_eq!(said(stars_back.into()), "A payment of 1 Star was refunded");
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

    fn text_message(text: &str, reply_to: Option<tl::enums::MessageReplyHeader>) -> tl::enums::Message {
        use grammers_tl_types::{Deserializable, Serializable};
        let mut bytes = Vec::new();
        0u32.serialize(&mut bytes);
        0u32.serialize(&mut bytes);
        9i32.serialize(&mut bytes);
        Who::User(7_002).peer().serialize(&mut bytes);
        1_700_000_000i32.serialize(&mut bytes);
        String::new().serialize(&mut bytes);
        let mut message = tl::types::Message::from_bytes(&bytes).expect("a blank message reads back");
        message.message = text.to_owned();
        message.reply_to = reply_to;
        message.from_id = Some(Who::User(7_003).peer());
        message.into()
    }

    fn quoted(text: Option<&str>, peer: Option<tl::enums::Peer>, from_name: Option<&str>) -> tl::enums::MessageReplyHeader {
        let reply_from = from_name.map(|name| {
            tl::types::MessageFwdHeader {
                imported: false,
                saved_out: false,
                from_id: None,
                from_name: Some(name.to_owned()),
                date: 1_700_000_000,
                channel_post: None,
                post_author: None,
                saved_from_peer: None,
                saved_from_msg_id: None,
                saved_from_id: None,
                saved_from_name: None,
                saved_date: None,
                psa_type: None,
            }
            .into()
        });
        tl::types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: false,
            quote: text.is_some(),
            reply_to_ephemeral: false,
            reply_to_msg_id: Some(4),
            reply_to_peer_id: peer,
            reply_from,
            reply_media: None,
            reply_to_top_id: None,
            quote_text: text.map(str::to_owned),
            quote_entities: None,
            quote_offset: Some(9),
            todo_item_id: None,
            poll_option: None,
        }
        .into()
    }

    #[test]
    fn a_partial_quote_is_the_reply_line_and_another_chat_keeps_its_own_words() {
        let dir = Directory::new(1);
        let whole = "Meet at the gate and bring the rope tomorrow, then wait";
        let partial = map(&dir, &text_message("On my way", Some(quoted(Some("bring the rope"), None, None)))).expect("a reply maps");
        let known = ("u7002".to_owned(), "Bea".to_owned(), whole.to_owned());
        let line = line_for(4, Some(&known), partial.reply_note.as_ref());
        assert_eq!(line.sender, "Bea");
        assert_eq!(line.snippet, "bring the rope");
        assert_ne!(line.snippet, whole.chars().take(120).collect::<String>());

        let other = Who::Channel(90).peer();
        let cross = map(&dir, &text_message("On my way", Some(quoted(Some("only this part"), Some(other), Some("Nia"))))).expect("a cross-chat reply maps");
        let note = cross.reply_note.as_ref().expect("the header's quote is kept");
        assert!(note.elsewhere);
        let line = line_for(4, Some(&known), Some(note));
        assert_eq!(line.sender, "Nia");
        assert_eq!(line.snippet, "only this part");
        assert!(!line.snippet.contains("Meet at the gate"));

        let story = tl::types::MessageReplyStoryHeader { peer: Who::User(7_004).peer(), story_id: 15 }.into();
        let told = map(&dir, &text_message("seen", Some(story))).expect("a story reply maps");
        let line = line_for(0, None, told.reply_note.as_ref());
        assert_eq!(line.id, "15");
        assert_eq!(line.snippet, "Story");
    }

    fn sticker_media(id: i64, mime: &str) -> (tl::types::MessageMediaDocument, tl::types::Document) {
        let document = tl::types::Document {
            id,
            access_hash: id + 1,
            file_reference: vec![8],
            date: 1_700_000_000,
            mime_type: mime.to_owned(),
            size: 30_000,
            thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".into(), w: 128, h: 128, size: 4_000 }.into()]),
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::types::DocumentAttributeSticker {
                mask: false,
                alt: "🥾".into(),
                stickerset: tl::types::InputStickerSetId { id: 900, access_hash: 901 }.into(),
                mask_coords: None,
            }
            .into()],
        };
        let media = tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: None,
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        };
        (media, document)
    }

    #[test]
    fn an_animated_sticker_names_the_lottie_file_and_keeps_the_still() {
        let (media, raw) = sticker_media(6_410, "application/x-tgsticker");
        let mut wants = Vec::new();
        let attachment = document(&media, &raw, &mut wants);
        assert!(attachment.sticker);
        assert_eq!(attachment.mime, "application/x-tgsticker");
        assert_eq!(attachment.file.id, "d6410", "the document is the file the row plays");
        assert_eq!(attachment.file.bytes, 30_000);
        assert_eq!(attachment.file.kind, MediaKind::Emoji, "a .tgs is unpacked the way a custom emoji is");
        assert_eq!(attachment.thumb.as_ref().map(|thumb| thumb.id.as_str()), Some("d6410m"));
        assert_eq!(attachment.thumb.as_ref().map(|thumb| thumb.bytes), Some(4_000));
        let animation = wants.iter().find(|want| want.file.id == "d6410").expect("the animation is fetched for the row");
        assert!(!animation.original, "a sticker is small enough to fetch with the row");
        assert!(wants.iter().any(|want| want.file.id == "d6410m" && !want.original));
    }

    #[test]
    fn a_video_sticker_names_the_webm_and_keeps_the_still() {
        let (media, raw) = sticker_media(6_411, "video/webm");
        let mut wants = Vec::new();
        let attachment = document(&media, &raw, &mut wants);
        assert!(attachment.sticker);
        assert_eq!(attachment.file.id, "d6411");
        assert_eq!(attachment.file.kind, MediaKind::Video);
        assert_eq!(attachment.file.bytes, 30_000);
        assert_eq!(attachment.thumb.as_ref().map(|thumb| thumb.id.as_str()), Some("d6411m"));
        let animation = wants.iter().find(|want| want.file.id == "d6411").expect("the webm is fetched for the row");
        assert!(!animation.original);
    }

    #[test]
    fn a_static_sticker_stays_its_thumbnail() {
        let (media, raw) = sticker_media(6_412, "image/webp");
        let mut wants = Vec::new();
        let attachment = document(&media, &raw, &mut wants);
        assert!(attachment.sticker);
        assert_eq!(attachment.file.id, "d6412m");
        assert!(attachment.thumb.as_ref().is_some_and(|thumb| thumb.id == "d6412m"));
        assert!(wants.iter().all(|want| want.file.id == "d6412m"));
    }

    use grammers_tl_types::Serializable;

    type Field<'a> = &'a dyn Fn(&mut Vec<u8>);

    fn blank<T: Deserializable>(fields: &[Field]) -> T {
        let mut bytes = Vec::new();
        fields.iter().for_each(|field| field(&mut bytes));
        T::from_bytes(&bytes).expect("a blank value reads back")
    }

    fn put<T: Serializable>(value: T) -> impl Fn(&mut Vec<u8>) {
        move |bytes| value.serialize(bytes)
    }

    fn wire_message(id: i32, text: &str) -> tl::types::Message {
        let mut message: tl::types::Message = blank(&[
            &put(0u32),
            &put(0u32),
            &put(id),
            &put(Who::User(7_002).peer()),
            &put(1_700_000_000i32),
            &put(text.to_owned()),
        ]);
        message.from_id = Some(Who::User(7_002).peer());
        message
    }

    fn photo_message(ttl: Option<i32>) -> tl::enums::Message {
        let mut message = wire_message(14, "");
        message.media = Some(
            tl::types::MessageMediaPhoto {
                spoiler: false,
                live_photo: false,
                photo: Some(
                    tl::types::Photo {
                        has_stickers: false,
                        id: 6_014,
                        access_hash: 6_015,
                        file_reference: vec![1],
                        date: 1_700_000_000,
                        sizes: vec![tl::types::PhotoSize { r#type: "m".into(), w: 320, h: 240, size: 4_000 }.into()],
                        video_sizes: None,
                        dc_id: 4,
                    }
                    .into(),
                ),
                ttl_seconds: ttl,
                video: None,
            }
            .into(),
        );
        message.into()
    }

    fn voice_message(ttl: Option<i32>) -> tl::enums::Message {
        let voice = tl::types::Document {
            id: 6_100,
            access_hash: 6_101,
            file_reference: vec![4],
            date: 1_700_000_000,
            mime_type: "audio/ogg".into(),
            size: 18_000,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::types::DocumentAttributeAudio {
                voice: true,
                duration: 7,
                title: None,
                performer: None,
                waveform: Some(vec![0b0001_1111]),
            }
            .into()],
        };
        let mut message = wire_message(15, "");
        message.media = Some(
            tl::types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: true,
                document: Some(voice.into()),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: ttl,
            }
            .into(),
        );
        message.into()
    }

    fn video_message(round: bool, ttl: Option<i32>) -> tl::enums::Message {
        let video = tl::types::Document {
            id: 6_200,
            access_hash: 6_201,
            file_reference: vec![5],
            date: 1_700_000_000,
            mime_type: "video/mp4".into(),
            size: 90_000,
            thumbs: Some(vec![tl::types::PhotoSize { r#type: "m".into(), w: 320, h: 180, size: 2_000 }.into()]),
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::types::DocumentAttributeVideo {
                round_message: round,
                supports_streaming: true,
                nosound: false,
                duration: 4.0,
                w: 320,
                h: 180,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            }
            .into()],
        };
        let mut message = wire_message(16, "");
        message.media = Some(
            tl::types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: !round,
                round,
                voice: false,
                document: Some(video.into()),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: ttl,
            }
            .into(),
        );
        message.into()
    }

    fn mapped(raw: &tl::enums::Message) -> Mapped {
        map(&Directory::new(7_001), raw).expect("the message maps")
    }

    #[test]
    fn a_view_once_photo_is_not_fetched_and_reads_as_locked() {
        prove_view_once().unwrap();
        let ordinary = mapped(&photo_message(None));
        assert!(!ordinary.wants.is_empty(), "an ordinary photo is still fetched");
        assert_eq!(ordinary.message.attachments.len(), 1);

        // Telegram's view-once mark is ttl_seconds = 0x7FFFFFFF. A shorter timer is the same
        // promise: the bytes are not a file this client may keep.
        let once = mapped(&photo_message(Some(i32::MAX)));
        assert!(once.wants.is_empty(), "a view-once photo queues no file");
        assert!(once.message.attachments.is_empty());
        assert!(once.message.text.contains("View-once photo"), "{}", once.message.text);
        assert!(matches!(once.message.kind, MessageKind::Unsupported { .. }));

        let timed = mapped(&photo_message(Some(30)));
        assert!(timed.wants.is_empty(), "a photo with a timer queues no file");
        assert!(timed.message.text.contains("Photo with a timer"), "{}", timed.message.text);

        let voice = mapped(&voice_message(Some(i32::MAX)));
        assert!(voice.wants.is_empty(), "a listen-once voice message queues no file");
        assert!(voice.message.attachments.is_empty());
        assert!(voice.message.text.contains("View-once voice message"), "{}", voice.message.text);

        let clip = mapped(&video_message(false, Some(i32::MAX)));
        assert!(clip.wants.is_empty(), "a view-once video queues no file");
        assert!(clip.message.text.contains("View-once video"), "{}", clip.message.text);

        let round = mapped(&video_message(true, Some(i32::MAX)));
        assert!(round.wants.is_empty(), "a view-once video message queues no file");
        assert!(round.message.text.contains("View-once video message"), "{}", round.message.text);
    }

    #[test]
    fn a_checklist_lists_its_items_and_marks_the_one_that_is_done() {
        prove_checklist().unwrap();
    }

    #[test]
    fn a_later_name_is_mentioned_when_an_earlier_at_sign_is_not_one() {
        let nameless = MentionTarget {
            name: "Ada Stone".into(),
            first: "Ada".into(),
            handle: String::new(),
            user_id: 7_003,
            access_hash: 7,
        };
        let named = MentionTarget {
            name: "Bea Moss".into(),
            first: "Bea".into(),
            handle: "bea_m".into(),
            user_id: 7_002,
            access_hash: 8,
        };
        // "hi" sits after both names. The address is not a mention, so it must not hide the later one.
        let text = "ping mail@Ada Stone see @Ada Stone and @Bea Moss hi";
        let (plain, entities) = apply_mentions(text, vec![bold(49, 2)], &[nameless, named]);
        assert_eq!(plain, "ping mail@Ada Stone see Ada and @bea_m hi");
        let mention = entities.iter().find_map(|entity| match entity {
            MessageEntity::InputMessageEntityMentionName(named) => Some(named),
            _ => None,
        });
        let mention = mention.expect("the person with no username is named");
        assert_eq!((mention.offset, mention.length), (24, 3));
        assert!(matches!(&mention.user_id, tl::enums::InputUser::User(user) if user.user_id == 7_003 && user.access_hash == 7));
        assert_eq!(entities.iter().filter(|entity| matches!(entity, MessageEntity::InputMessageEntityMentionName(_))).count(), 1);
        let bold = entities.iter().find_map(|entity| match entity {
            MessageEntity::Bold(bold) => Some((bold.offset, bold.length)),
            _ => None,
        });
        // "@Ada Stone" (10) became "Ada" (3) and "@Bea Moss" (9) became "@bea_m" (6): "hi" moves left by 10.
        assert_eq!(bold, Some((39, 2)));
        let shown = spans(&plain, &entities);
        assert!(shown.iter().any(|span| {
            matches!(&span.style, Style::Mention { kind: MentionKind::User, id } if id == "u7003")
                && &plain[span.start as usize..span.end as usize] == "Ada"
        }));
    }

    #[test]
    fn explicit_mention_spans_produce_input_mention_name_entities() {
        // Emoji before the mention takes 2 UTF-16 units (4 UTF-8 bytes), verifying UTF-16 offset conversion.
        let text = "👋 @Ada Stone!";
        let start = text.find("@Ada Stone").unwrap() as u32;
        let end = start + "@Ada Stone".len() as u32;
        let entities = apply_mention_spans(
            text,
            Vec::new(),
            &[Span {
                start,
                end,
                style: Style::Mention { kind: MentionKind::User, id: "u7003".into() },
            }],
            |uid| (uid == 7_003).then_some(99),
        );
        assert_eq!(entities.len(), 1);
        match &entities[0] {
            MessageEntity::InputMessageEntityMentionName(named) => {
                // "👋 " is 2 + 1 = 3 UTF-16 units; "@Ada Stone" is 10 UTF-16 units.
                assert_eq!((named.offset, named.length), (3, 10));
                assert!(matches!(
                    &named.user_id,
                    tl::enums::InputUser::User(user) if user.user_id == 7_003 && user.access_hash == 99
                ));
            }
            other => panic!("expected InputMessageEntityMentionName, got {other:?}"),
        }
    }

    #[test]
    fn shared_contact_maps_media_contact_with_and_without_user_id() {
        let dir = Directory::new(7_001);
        let make_msg = |id: i32, media: MessageMedia| -> tl::enums::Message {
            use grammers_tl_types::{Deserializable, Serializable};
            let mut bytes = Vec::new();
            0u32.serialize(&mut bytes);
            0u32.serialize(&mut bytes);
            id.serialize(&mut bytes);
            Who::User(7_002).peer().serialize(&mut bytes);
            1_700_000_000i32.serialize(&mut bytes);
            String::new().serialize(&mut bytes);
            let mut msg = tl::types::Message::from_bytes(&bytes).expect("blank message");
            msg.from_id = Some(Who::User(7_002).peer());
            msg.media = Some(media);
            msg.into()
        };

        let with_user = make_msg(
            10,
            tl::types::MessageMediaContact {
                phone_number: "15550100777".into(),
                first_name: "Odalys".into(),
                last_name: "Brandt".into(),
                vcard: "BEGIN:VCARD\nVERSION:3.0\nORG:Trail Crew\nEMAIL;TYPE=INTERNET:odalys@example.org\nEND:VCARD".into(),
                user_id: 7_002,
            }
            .into(),
        );
        let mapped = map(&dir, &with_user).expect("mapped contact");
        assert_eq!(mapped.message.kind, MessageKind::Normal, "shared contact must not be Unsupported");
        assert_eq!(mapped.message.contacts.len(), 1);
        let card = &mapped.message.contacts[0];
        assert_eq!(card.name, "Odalys Brandt");
        assert_eq!(card.phones, vec!["+15550100777"]);
        assert_eq!(card.emails, vec!["odalys@example.org"]);
        assert_eq!(card.organization, "Trail Crew");
        assert_eq!(card.person_id.as_deref(), Some("u7002"));

        let without_user = make_msg(
            11,
            tl::types::MessageMediaContact {
                phone_number: "+15550100999".into(),
                first_name: "External".into(),
                last_name: String::new(),
                vcard: String::new(),
                user_id: 0,
            }
            .into(),
        );
        let mapped_ext = map(&dir, &without_user).expect("mapped external contact");
        assert_eq!(mapped_ext.message.kind, MessageKind::Normal);
        assert_eq!(mapped_ext.message.contacts.len(), 1);
        let ext = &mapped_ext.message.contacts[0];
        assert_eq!(ext.name, "External");
        assert_eq!(ext.phones, vec!["+15550100999"]);
        assert_eq!(ext.person_id, None);
    }
}
