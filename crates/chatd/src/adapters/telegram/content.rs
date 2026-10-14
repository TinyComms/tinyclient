//! Telegram's messages into the one message model: entities to spans, media to attachments,
//! service messages to one grey line, and whatever tinyclient cannot draw to a named line with a
//! link into Telegram's own app. Nothing here asks the network anything.

use super::peers::{message_link, Directory, Who};
use grammers_tl_types as tl;
use protocol::{
    Attachment, Forwarded, MediaFile, MediaKind, MentionKind, Message, MessageKind, Preview, Reaction, Sender, Span,
    Style,
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
    /// A picture itself, for the viewer: kept until the shell asks for it, never fetched for a
    /// row.
    pub original: bool,
}

pub struct Mapped {
    pub peer: Who,
    pub id: i32,
    /// The forum topic its reply header names. `None` in a forum means the General topic.
    pub topic: Option<i32>,
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
    let mut thumb = MediaFile::new(format!("p{}{kind}", photo.id), MediaKind::Image);
    (thumb.width, thumb.height, thumb.bytes) = (width, height, bytes);
    wants.push(Want { file: thumb.clone(), dc: photo.dc_id, location: location(kind), inline: inline.map(<[u8]>::to_vec), row: None, original: false });
    // The picture itself is the largest size Telegram keeps: the viewer fills a screen with
    // it, at the screen's own pixels.
    let mut file = thumb.clone();
    if let Some((kind, width, height, bytes, inline)) = pick(&photo.sizes, u32::MAX).filter(|(largest, ..)| *largest != kind) {
        file.id = format!("p{}{kind}", photo.id);
        (file.width, file.height, file.bytes) = (width, height, bytes);
        wants.push(Want { file: file.clone(), dc: photo.dc_id, location: location(kind), inline: inline.map(<[u8]>::to_vec), row: None, original: true });
    }
    let mut attachment = Attachment::new("Photo", file);
    attachment.thumb = Some(thumb);
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
    } else if kind == MediaKind::Image && still.is_some() {
        // A picture sent as a file opens in the viewer like a photo, so it is kept the same way.
        let location = tl::types::InputDocumentFileLocation {
            id: document.id,
            access_hash: document.access_hash,
            file_reference: document.file_reference.clone(),
            thumb_size: String::new(),
        };
        wants.push(Want { file: file.clone(), dc: document.dc_id, location: location.into(), inline: None, row: None, original: true });
    }
    let mut attachment = Attachment::new(if name.is_empty() { label.to_owned() } else { name }, file);
    attachment.thumb = still;
    attachment.mime = mime.to_owned();
    attachment.duration = duration;
    attachment.spoiler = media.spoiler;
    attachment.sticker = sticker;
    attachment.waveform = wave;
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

/// A service message as one line. `who` did it; "You" when it was us.
fn action(dir: &Directory, who: &str, action: &MessageAction) -> String {
    let name = |id: i64| if id == dir.me { "you".to_owned() } else { dir.name(Who::User(id)) };
    let names = |ids: &[i64]| ids.iter().map(|id| name(*id)).collect::<Vec<_>>().join(", ");
    use MessageAction as A;
    match action {
        A::ChatCreate(a) => format!("{who} created the group \"{}\"", a.title),
        A::ChannelCreate(a) => format!("\"{}\" was created", a.title),
        A::ChatEditTitle(a) => format!("{who} renamed this to \"{}\"", a.title),
        A::ChatEditPhoto(_) => format!("{who} changed the picture"),
        A::ChatDeletePhoto => format!("{who} removed the picture"),
        A::ChatAddUser(a) if a.users.len() == 1 && name(a.users[0]) == who.to_lowercase() => format!("{who} joined"),
        A::ChatAddUser(a) => format!("{who} added {}", names(&a.users)),
        A::ChatDeleteUser(a) if name(a.user_id) == who.to_lowercase() || dir.name(Who::User(a.user_id)) == who => format!("{who} left"),
        A::ChatDeleteUser(a) => format!("{who} removed {}", name(a.user_id)),
        A::ChatJoinedByLink(_) => format!("{who} joined with an invite link"),
        A::ChatJoinedByRequest => format!("{who} was let in"),
        A::ChatMigrateTo(_) | A::ChannelMigrateFrom(_) => "This group was upgraded to a supergroup".to_owned(),
        A::PinMessage => format!("{who} pinned a message"),
        A::HistoryClear => "History was cleared".to_owned(),
        A::PhoneCall(call) => {
            let kind = if call.video { "Video call" } else { "Call" };
            match call.duration {
                Some(seconds) => format!("{kind}, {}", duration_words(seconds)),
                None => format!("{kind}, not answered"),
            }
        }
        A::GroupCall(call) => match call.duration {
            Some(seconds) => format!("Video chat ended, {}", duration_words(seconds)),
            None => format!("{who} started a video chat"),
        },
        A::ScreenshotTaken => format!("{who} took a screenshot"),
        A::ContactSignUp => format!("{who} joined Telegram"),
        A::TopicCreate(topic) => format!("{who} created the topic \"{}\"", topic.title),
        A::TopicEdit(edit) => match (&edit.title, edit.closed) {
            (Some(title), _) => format!("{who} renamed the topic to \"{title}\""),
            (None, Some(true)) => format!("{who} closed the topic"),
            (None, Some(false)) => format!("{who} reopened the topic"),
            (None, None) => format!("{who} changed the topic"),
        },
        A::SetMessagesTtl(_) => format!("{who} changed how long messages are kept"),
        A::CustomAction(custom) => custom.message.clone(),
        _ => format!("{who} changed something tinyclient cannot show yet"),
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
            let mut message = Message::plain(service.id.to_string(), "", action(dir, &who, &service.action), i64::from(service.date), outgoing);
            message.sender = sender(dir, from);
            message.kind = MessageKind::System;
            Some(Mapped {
                peer,
                id: service.id,
                topic: topic_of(service.reply_to.as_ref()).0,
                reply: None,
                message,
                wants: Vec::new(),
            })
        }
        tl::enums::Message::Message(raw) => {
            let peer = Who::of(&raw.peer_id);
            // No author: in a direct chat it is one of the two; a channel's post is the channel's.
            let from = match (&raw.from_id, peer) {
                (Some(from), _) => Who::of(from),
                (None, Who::User(_)) if raw.out => Who::User(dir.me),
                (None, peer) => peer,
            };
            let outgoing = raw.out || (from == Who::User(dir.me) && peer == Who::User(dir.me));
            let mut message = Message::plain(raw.id.to_string(), "", raw.message.clone(), i64::from(raw.date), outgoing);
            message.sender = sender(dir, from);
            message.spans = spans(&raw.message, raw.entities.as_deref().unwrap_or(&[]));
            message.edited = raw.edit_date.is_some() && !raw.edit_hide;
            message.pinned = raw.pinned;
            message.mentions_me = raw.mentioned;
            message.reactions = raw.reactions.as_ref().map(reactions).unwrap_or_default();
            message.forwarded = raw.fwd_from.as_ref().map(|header| Box::new(forwarded(dir, header)));
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
                // The dice is its own emoji; the roll is animated in Telegram's app only.
                Some(MessageMedia::Dice(dice)) => message.text = dice.emoticon.clone(),
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
            Some(Mapped {
                peer,
                id: raw.id,
                topic,
                reply,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
