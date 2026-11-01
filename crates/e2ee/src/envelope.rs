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
const VERSION: u64 = 1;

/// Which row of the server's table a ciphertext is: a message, or an edit of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Message,
    Edit,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Edit => "edit",
        }
    }

    pub fn parse(text: &str) -> Result<Kind> {
        match text {
            "message" => Ok(Kind::Message),
            "edit" => Ok(Kind::Edit),
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
}

/// A message or an edit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Envelope {
    pub text: String,
    /// The id of the message this one answers. Messages only.
    pub reply_to: Option<String>,
    pub attachments: Vec<AttachmentRef>,
    /// Milliseconds since the Unix epoch, as the sender's clock said. Messages only. Unverified:
    /// the server's `created_at` is the order.
    pub sent_at: Option<i64>,
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
}

#[derive(Serialize, Deserialize)]
struct Wire {
    v: u64,
    #[serde(default)]
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    #[serde(default)]
    attachments: Vec<WireAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_at: Option<i64>,
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

    /// Encodes for `kind`. An edit carries `v`, `text` and `attachments` only.
    pub fn encode(&self, kind: Kind) -> Result<Vec<u8>> {
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::TooLarge("attachment list"));
        }
        let attachments = self
            .attachments
            .iter()
            .map(|attachment| {
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
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let wire = Wire {
            v: VERSION,
            text: self.text.clone(),
            reply_to: if kind == Kind::Message {
                self.reply_to.clone()
            } else {
                None
            },
            attachments,
            sent_at: if kind == Kind::Message {
                self.sent_at
            } else {
                None
            },
        };
        let bytes = serde_json::to_vec(&wire).map_err(|_| Error::Malformed("envelope"))?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(Error::TooLarge("message"));
        }
        Ok(bytes)
    }

    /// Decodes plaintext received for `kind`.
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
        let attachments = wire
            .attachments
            .into_iter()
            .map(|attachment| {
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
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let (reply_to, sent_at) = match kind {
            Kind::Message => (wire.reply_to, wire.sent_at),
            Kind::Edit => (None, None),
        };
        Ok(Envelope {
            text: wire.text,
            reply_to,
            attachments,
            sent_at,
        })
    }
}
