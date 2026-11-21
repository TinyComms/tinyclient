//! The plaintext inside an MLS application message (`docs/e2ee-text.md`, section 5): JSON,
//! UTF-8, at most 16 KiB.
//!
//! ```json
//! { "v": 1, "text": "...", "reply_to": "<id>", "attachments": [
//!   { "id": "...", "key": "<b64 32 bytes>", "name": "...", "mime": "...", "bytes": 12345,
//!     "sha256": "<b64 32 bytes, of the uploaded ciphertext>", "width": 0, "height": 0 } ],
//!   "sent_at": 1730000000000 }
//! ```
//!
//! The codec is strict about what it understands and tolerant of what it does not: unknown
//! fields are ignored (so a later version can add them), a field of the wrong type is an error, a
//! duplicate field is an error, anything but `"v": 1` is [`Error::UnsupportedVersion`], the size
//! limits are enforced on both sides, and the decoder never panics, whatever the bytes.

use crate::attachment::AttachmentKey;
use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// The most an envelope may be, encoded.
pub const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
/// The most attachments one message may carry.
pub const MAX_ATTACHMENTS: usize = 10;
const MAX_ID: usize = 128;
const MAX_NAME: usize = 255;
const MAX_MIME: usize = 127;
const MAX_WAVEFORM: usize = 100;
const MAX_MENTIONS: usize = 50;
const VERSION: u64 = 1;

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Which row of the server's table a ciphertext is: a message, an edit of one, or a reaction to
/// one. A reaction names its message only inside the ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Message,
    Edit,
    Reaction,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Edit => "edit",
            Kind::Reaction => "reaction",
        }
    }

    pub fn parse(text: &str) -> Result<Kind> {
        match text {
            "message" => Ok(Kind::Message),
            "edit" => Ok(Kind::Edit),
            "reaction" => Ok(Kind::Reaction),
            _ => Err(Error::Malformed("kind")),
        }
    }
}

/// An attachment as the message names it: how to fetch it and how to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// The server's id of the sealed blob (`attachment_ids`, `GET /v1/media/{id}`).
    pub id: String,
    /// The per-attachment AES-256 key. Never leaves the message.
    pub key: AttachmentKey,
    pub name: String,
    pub mime: String,
    /// Length of the plaintext file. Only the message carries it: the sealed blob is padded
    /// (Padme), and opening needs this to tell the file from its padding.
    pub bytes: u64,
    /// SHA-256 of the sealed blob as uploaded.
    pub sha256: [u8; 32],
    pub width: u32,
    pub height: u32,
    /// Duration in seconds for audio/video/voice attachments.
    pub duration: u32,
    /// True when this attachment is a recorded voice message.
    pub voice: bool,
    /// Voice message loudness bars (0..=255), at most 100 entries.
    pub waveform: Vec<u8>,
}

/// A structured @mention carried inside a sealed envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionRef {
    /// `"user"`, `"role"`, or `"everyone"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub start: u32,
    pub end: u32,
}

/// A custom emoji span carried inside a sealed envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiRef {
    pub id: String,
    pub start: u32,
    pub end: u32,
}

/// At most this many link preview cards in one sealed message.
pub const MAX_PREVIEWS: usize = 3;

/// A sender-built link preview card carried inside a sealed envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRef {
    pub url: String,
    pub site: String,
    pub title: String,
    pub description: String,
    pub image: Option<AttachmentRef>,
}

/// A message or an edit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Envelope {
    pub text: String,
    /// The id of the message this one answers. Messages only.
    pub reply_to: Option<String>,
    /// The root message id when this message is a thread reply. Messages only.
    pub thread_id: Option<String>,
    pub attachments: Vec<AttachmentRef>,
    /// Structured @mentions in `text` (UTF-8 byte offsets). Messages and edits.
    pub mentions: Vec<MentionRef>,
    /// Community custom emoji spans in `text` (UTF-8 byte offsets). Messages and edits.
    pub emoji: Vec<EmojiRef>,
    /// Community sticker id when the message is a sticker. Messages only.
    pub sticker_id: Option<String>,
    /// Sender-built link preview cards. Messages only.
    pub previews: Vec<PreviewRef>,
    /// Milliseconds since the Unix epoch, as the sender's clock said. Messages only. Unverified:
    /// the server's `created_at` is the order.
    pub sent_at: Option<i64>,
    /// A reaction only: `true` adds it, `false` takes it back. A reaction's `text` is the emoji
    /// and its `reply_to` the message it lands on; the server never sees either.
    pub on: bool,
}

#[derive(Serialize, Deserialize)]
struct WireAttachment {
    id: String,
    key: String,
    name: String,
    mime: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    duration: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    voice: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waveform: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct WirePreview {
    url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    site: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<WireAttachment>,
}

#[derive(Serialize, Deserialize)]
struct Wire {
    v: u64,
    #[serde(default)]
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(default)]
    attachments: Vec<WireAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mentions: Vec<MentionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    emoji: Vec<EmojiRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sticker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    previews: Vec<WirePreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_at: Option<i64>,
    /// Reactions only, and always written there: absent means nothing on a message or an edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on: Option<bool>,
}

fn encode_attachment(attachment: &AttachmentRef) -> Result<WireAttachment> {
    if attachment.id.len() > MAX_ID
        || attachment.name.len() > MAX_NAME
        || attachment.mime.len() > MAX_MIME
    {
        return Err(Error::TooLarge("attachment field"));
    }
    Ok(WireAttachment {
        id: attachment.id.clone(),
        key: STANDARD.encode(attachment.key.as_bytes()),
        name: attachment.name.clone(),
        mime: attachment.mime.clone(),
        bytes: attachment.bytes,
        sha256: STANDARD.encode(attachment.sha256),
        width: attachment.width,
        height: attachment.height,
        duration: attachment.duration,
        voice: attachment.voice,
        waveform: attachment.waveform.iter().copied().take(MAX_WAVEFORM).collect(),
    })
}

fn decode_attachment(attachment: WireAttachment) -> Result<AttachmentRef> {
    if attachment.id.is_empty()
        || attachment.id.len() > MAX_ID
        || attachment.name.len() > MAX_NAME
        || attachment.mime.len() > MAX_MIME
    {
        return Err(Error::Malformed("attachment"));
    }
    Ok(AttachmentRef {
        key: AttachmentKey::from_bytes(decode_b32(&attachment.key, "attachment key")?),
        sha256: decode_b32(&attachment.sha256, "attachment hash")?,
        id: attachment.id,
        name: attachment.name,
        mime: attachment.mime,
        bytes: attachment.bytes,
        width: attachment.width,
        height: attachment.height,
        duration: attachment.duration,
        voice: attachment.voice,
        waveform: attachment.waveform.iter().copied().take(MAX_WAVEFORM).collect(),
    })
}

fn decode_b32(text: &str, what: &'static str) -> Result<[u8; 32]> {
    let bytes = STANDARD.decode(text).map_err(|_| Error::Malformed(what))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| Error::Malformed(what))
}

impl Envelope {
    pub fn message(text: impl Into<String>) -> Envelope {
        Envelope {
            text: text.into(),
            sent_at: None,
            ..Envelope::default()
        }
    }

    /// Encodes for `kind`. An edit carries `v`, `text` and `attachments` only; a reaction
    /// carries `v`, `text`, `reply_to` and `on`.
    pub fn encode(&self, kind: Kind) -> Result<Vec<u8>> {
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::TooLarge("attachment list"));
        }
        if kind == Kind::Reaction
            && (self.reply_to.is_none() || self.text.is_empty() || !self.attachments.is_empty())
        {
            return Err(Error::Malformed("reaction"));
        }
        let on = match kind {
            Kind::Reaction => Some(self.on),
            Kind::Message | Kind::Edit => None,
        };
        let attachments = self
            .attachments
            .iter()
            .map(encode_attachment)
            .collect::<Result<Vec<_>>>()?;
        let previews = if kind == Kind::Message {
            self.previews
                .iter()
                .take(MAX_PREVIEWS)
                .map(|preview| {
                    Ok(WirePreview {
                        url: preview.url.clone(),
                        site: preview.site.clone(),
                        title: preview.title.clone(),
                        description: preview.description.clone(),
                        image: preview.image.as_ref().map(encode_attachment).transpose()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let wire = Wire {
            v: VERSION,
            text: self.text.clone(),
            reply_to: match kind {
                Kind::Message | Kind::Reaction => self.reply_to.clone(),
                Kind::Edit => None,
            },
            thread_id: if kind == Kind::Message {
                self.thread_id.clone()
            } else {
                None
            },
            attachments,
            mentions: if kind == Kind::Reaction {
                Vec::new()
            } else {
                self.mentions.iter().take(MAX_MENTIONS).cloned().collect()
            },
            emoji: if kind == Kind::Reaction {
                Vec::new()
            } else {
                self.emoji.iter().take(MAX_MENTIONS).cloned().collect()
            },
            sticker_id: if kind == Kind::Message {
                self.sticker_id.clone()
            } else {
                None
            },
            previews,
            sent_at: if kind == Kind::Message {
                self.sent_at
            } else {
                None
            },
            on,
        };
        let bytes = serde_json::to_vec(&wire).map_err(|_| Error::Malformed("envelope"))?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(Error::TooLarge("message"));
        }
        Ok(bytes)
    }

    /// Decodes plaintext received for `kind`. A reaction must carry `reply_to`, `on` and no
    /// attachments, exactly as [`Envelope::encode`] wrote it.
    pub fn decode(kind: Kind, bytes: &[u8]) -> Result<Envelope> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(Error::TooLarge("message"));
        }
        // UTF-8 first, so every later error is about JSON and not about bytes.
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Malformed("message encoding"))?;
        let wire: Wire = serde_json::from_str(text).map_err(|_| Error::Malformed("message"))?;
        if wire.v != VERSION {
            return Err(Error::UnsupportedVersion);
        }
        if wire.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::TooLarge("attachment list"));
        }
        if wire.reply_to.as_ref().is_some_and(|id| id.len() > MAX_ID) {
            return Err(Error::TooLarge("reply_to"));
        }
        if kind == Kind::Reaction
            && (wire.reply_to.is_none() || wire.on.is_none() || !wire.attachments.is_empty())
        {
            return Err(Error::Malformed("reaction"));
        }
        let attachments = wire
            .attachments
            .into_iter()
            .map(decode_attachment)
            .collect::<Result<Vec<_>>>()?;
        let previews = if kind == Kind::Message {
            wire.previews
                .into_iter()
                .take(MAX_PREVIEWS)
                .map(|preview| {
                    Ok(PreviewRef {
                        url: preview.url,
                        site: preview.site,
                        title: preview.title,
                        description: preview.description,
                        image: preview.image.map(decode_attachment).transpose()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let (reply_to, sent_at, on) = match kind {
            Kind::Message => (wire.reply_to, wire.sent_at, false),
            Kind::Edit => (None, None, false),
            Kind::Reaction => (wire.reply_to, None, wire.on.expect("checked above")),
        };
        let mentions = if kind == Kind::Reaction {
            Vec::new()
        } else {
            wire.mentions.into_iter().take(MAX_MENTIONS).collect()
        };
        let emoji = if kind == Kind::Reaction {
            Vec::new()
        } else {
            wire.emoji.into_iter().take(MAX_MENTIONS).collect()
        };
        let sticker_id = if kind == Kind::Message {
            wire.sticker_id.filter(|id| !id.is_empty() && id.len() <= MAX_ID)
        } else {
            None
        };
        let thread_id = if kind == Kind::Message {
            wire.thread_id.filter(|id| !id.is_empty() && id.len() <= MAX_ID)
        } else {
            None
        };
        Ok(Envelope {
            text: wire.text,
            reply_to,
            thread_id,
            attachments,
            mentions,
            emoji,
            sticker_id,
            previews,
            sent_at,
            on,
        })
    }
}
