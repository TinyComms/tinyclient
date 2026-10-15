//! What a message can hold, the same for every network. Adapters translate into this; the shell
//! never sees Slack mrkdwn, Discord markdown, Telegram entities or Teams HTML.

use crate::{is_false, is_zero, Key};
use serde::{Deserialize, Serialize};

/// Most people a [`ThreadSummary`] names, for the pictures beside its reply count.
pub const THREAD_FACES: usize = 3;
/// Most fields a [`Card`] holds.
pub const CARD_FIELDS: usize = 10;
/// Most bytes of a voice message's waveform.
pub const WAVEFORM_LIMIT: usize = 256;

/// Cuts text from a network to at most `chars` characters, so no frame grows with what a
/// stranger typed.
pub fn clip(text: &mut String, chars: usize) {
    if let Some((at, _)) = text.char_indices().nth(chars) {
        text.truncate(at);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A person's or group's picture, at list size.
    Avatar,
    /// A Space's icon.
    Icon,
    /// A custom emoji.
    Emoji,
    /// A still picture: an image, or the poster of a video or GIF, no wider than 800 px.
    Image,
    Video,
    Gif,
    Audio,
    /// Anything else. Fetched only when the user asks.
    File,
}

/// A file chatd can put on disk. The socket carries the path, never the bytes, and chatd never
/// decodes it. `width` and `height` are known before the bytes arrive, so rows never jump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFile {
    /// Unique within the account and stable: an attachment id, or the network's avatar hash. A
    /// changed picture is a new id.
    pub id: String,
    pub kind: MediaKind,
    /// Pixels. 0 when the network did not say.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: u32,
    /// Size of the file. 0 when unknown.
    #[serde(default)]
    pub bytes: u64,
    /// Where the file is, once it is on disk. `None`: ask `Fetch` when it is on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why the file could not be fetched, in plain words. While this is set chatd does not try
    /// again and the shell should stop asking; `Fetch` with `retry` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl MediaFile {
    pub fn new(id: impl Into<String>, kind: MediaKind) -> MediaFile {
        MediaFile {
            id: id.into(),
            kind,
            width: 0,
            height: 0,
            bytes: 0,
            path: None,
            failed: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionKind {
    User,
    Channel,
    Role,
    /// Everyone, here, channel.
    Everyone,
}

/// How a range of a message's text is drawn. Block styles (`CodeBlock`, `Quote`, `ListItem`)
/// cover whole lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum Style {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    CodeBlock {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        language: String,
    },
    Quote,
    ListItem {
        ordered: bool,
        depth: u8,
    },
    Spoiler,
    Link {
        url: String,
    },
    /// The text already reads `@Priya` or `#deploys`; `id` is who or what it names.
    Mention {
        kind: MentionKind,
        id: String,
    },
    /// A custom emoji. The text reads `:name:`, which is also the fallback.
    Emoji {
        file: MediaFile,
    },
}

/// A styled range of [`Message::text`]. `start` and `end` are byte offsets into the UTF-8 text
/// and always fall on character boundaries. Ranges may nest and overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// The file itself: its `bytes` is the size shown on a file row.
    pub file: MediaFile,
    /// What an image, video or GIF shows inline. Small enough to fetch when on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime: String,
    /// Seconds, for audio and video.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duration: u32,
    /// Hidden until the user asks to see it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub spoiler: bool,
    /// A sticker: drawn at its own size with no frame, and its transparency kept.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sticker: bool,
    /// A voice message's loudness over time, one byte a bar, at most [`WAVEFORM_LIMIT`]. Its
    /// length in seconds is `duration`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waveform: Vec<u8>,
    /// A round video message (Telegram's): drawn in a circle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub round: bool,
}

impl Attachment {
    /// An attachment with nothing said about it but its name and file.
    pub fn new(name: impl Into<String>, file: MediaFile) -> Attachment {
        Attachment {
            name: name.into(),
            file,
            thumb: None,
            mime: String::new(),
            duration: 0,
            spoiler: false,
            sticker: false,
            waveform: Vec::new(),
            round: false,
        }
    }
}

/// A link's card, as the network built it. tinyclient never fetches the link itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub site: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Only when the network serves it from its own proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MediaFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// The emoji itself, or `:name:` for a custom one.
    pub emoji: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<MediaFile>,
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mine: bool,
}

/// The earlier message this one answers, as one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub id: String,
    pub sender: String,
    pub snippet: String,
    /// Who wrote it, for their picture. `None` for a webhook, or when the network did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<Key>,
    /// The message answered has been deleted. `snippet` is then empty.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

/// The thread hanging off this message. Its replies are not in the conversation's own pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// The thread's conversation id: what the thread pane asks `Messages` for.
    pub id: String,
    pub replies: u32,
    /// Unix seconds of the newest reply.
    pub last: i64,
    /// The latest people to reply, newest first, at most [`THREAD_FACES`], for their pictures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<Key>,
    /// Replies the user has not read. 0 when the network does not say per thread.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unread: u32,
}

/// Where a forwarded message came from. What was forwarded is the message's own text and
/// attachments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forwarded {
    /// The conversation it was first said in: `#announcements`, a person's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_title: String,
    /// That conversation's Space, when it has one and it is not this one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_space: String,
    /// Opens the original in the network's own app. Empty when it cannot be linked to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
}

/// What a bot or app posts instead of plain text: an embed, a block of fields. Drawn as one
/// framed card under the message's text. Every string is cut to a fixed length on its way in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Label and value, in the network's order, at most [`CARD_FIELDS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub footer: String,
    /// The accent the network gave it, `0xRRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Normal,
    /// One grey line: joined, pinned, call started. `text` says it.
    System,
    /// Something tinyclient cannot draw. `text` names it ("Poll: Lunch?") and `link` opens it in the
    /// official app. Never blank, never dropped.
    Unsupported {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        link: String,
    },
}

impl MessageKind {
    fn is_normal(&self) -> bool {
        *self == MessageKind::Normal
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SendState {
    /// On the network. Every incoming message is this.
    #[default]
    Sent,
    /// On its way. The message's `id` is its `client_id` until the network names it.
    Pending,
    /// It did not go, and will not unless the user asks. `reason` is in words.
    Failed { reason: String },
}

impl SendState {
    fn is_sent(&self) -> bool {
        *self == SendState::Sent
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The person's id on the account. Empty for a webhook or an app with no user.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<MediaFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
}

/// One message. Which conversation it is in comes from the answer that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub sender: Sender,
    /// Unix seconds.
    pub sent: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub outgoing: bool,
    /// Plain text. Mentions already read as names and no network markup is left in it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previews: Vec<Preview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadSummary>,
    /// Boxed, like `card`: few messages have one, and every message would carry its size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<Box<Forwarded>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Box<Card>>,
    /// A message that was deleted but still has a thread hanging off it. Everything it said
    /// is gone; the row stays so the thread has somewhere to hang.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mentions_me: bool,
    #[serde(default, skip_serializing_if = "MessageKind::is_normal")]
    pub kind: MessageKind,
    #[serde(default, skip_serializing_if = "SendState::is_sent")]
    pub state: SendState,
    /// What the shell called it when sending. Set on our own messages only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl Message {
    /// A message that is only text, which is what an adapter has before it maps anything richer.
    pub fn plain(
        id: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
        sent: i64,
        outgoing: bool,
    ) -> Message {
        Message {
            id: id.into(),
            sender: Sender {
                name: sender.into(),
                ..Sender::default()
            },
            sent,
            outgoing,
            text: text.into(),
            spans: Vec::new(),
            attachments: Vec::new(),
            previews: Vec::new(),
            reactions: Vec::new(),
            reply_to: None,
            thread: None,
            forwarded: None,
            card: None,
            deleted: false,
            edited: false,
            pinned: false,
            mentions_me: false,
            kind: MessageKind::Normal,
            state: SendState::Sent,
            client_id: None,
        }
    }

    /// Cuts everything a network could make arbitrarily large to its limit. The store does this
    /// to every message on its way in, so no adapter has to remember to.
    pub fn bound(&mut self) {
        for attachment in &mut self.attachments {
            attachment.waveform.truncate(WAVEFORM_LIMIT);
        }
        if let Some(thread) = &mut self.thread {
            thread.participants.truncate(THREAD_FACES);
        }
        if let Some(forwarded) = &mut self.forwarded {
            clip(&mut forwarded.from_title, 200);
            clip(&mut forwarded.from_space, 200);
            clip(&mut forwarded.link, 2_000);
        }
        if let Some(card) = &mut self.card {
            clip(&mut card.title, 256);
            clip(&mut card.text, 4_000);
            clip(&mut card.footer, 256);
            card.fields.truncate(CARD_FIELDS);
            for (label, value) in &mut card.fields {
                clip(label, 256);
                clip(value, 1_000);
            }
        }
    }

    /// What is left of a message once it is deleted but must stay: who and when, nothing else.
    pub fn tombstone(&mut self) {
        self.text.clear();
        self.spans.clear();
        self.attachments.clear();
        self.previews.clear();
        self.reactions.clear();
        self.reply_to = None;
        self.forwarded = None;
        self.card = None;
        self.pinned = false;
        self.mentions_me = false;
        self.deleted = true;
    }

    /// Every file the message refers to, for registering and for fetching what is on screen.
    pub fn files(&self) -> impl Iterator<Item = &MediaFile> {
        let attached = self
            .attachments
            .iter()
            .flat_map(|attachment| std::iter::once(&attachment.file).chain(&attachment.thumb));
        let emoji = self.spans.iter().filter_map(|span| match &span.style {
            Style::Emoji { file } => Some(file),
            _ => None,
        });
        attached
            .chain(self.previews.iter().filter_map(|preview| preview.image.as_ref()))
            .chain(self.reactions.iter().filter_map(|reaction| reaction.file.as_ref()))
            .chain(emoji)
            .chain(&self.sender.avatar)
    }

    /// The same, to fill in paths.
    pub fn files_mut(&mut self) -> impl Iterator<Item = &mut MediaFile> {
        let attached = self
            .attachments
            .iter_mut()
            .flat_map(|attachment| std::iter::once(&mut attachment.file).chain(&mut attachment.thumb));
        let emoji = self.spans.iter_mut().filter_map(|span| match &mut span.style {
            Style::Emoji { file } => Some(file),
            _ => None,
        });
        attached
            .chain(self.previews.iter_mut().filter_map(|preview| preview.image.as_mut()))
            .chain(self.reactions.iter_mut().filter_map(|reaction| reaction.file.as_mut()))
            .chain(emoji)
            .chain(&mut self.sender.avatar)
    }
}
