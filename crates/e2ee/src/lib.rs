//! Client-side end-to-end encryption for tinyserver's encrypted text and attachments. The contract
//! is `docs/e2ee-text.md` in tinyserver; where this crate reads it ambiguously, the reading and the
//! proposed wire change are in `docs/e2ee-core-notes.md`.
//!
//! A pure library: no network, no async runtime, no GUI. The only files it writes are the ones a
//! [`store::Store`] it is given writes and the private temp file of [`attachment::decrypt_to_temp`].
//!
//! * [`identity`]: a device's P-256 identity, cross-signatures, and the verifier for the device lists
//!   the server hands out.
//! * [`engine::Engine`] (with [`groups`] and [`messaging`]): MLS groups on openmls, key packages,
//!   commits and Welcomes, message encryption.
//! * [`envelope`]: the JSON inside a message.
//! * [`attachment`]: the sealed blob format, as streams.
//! * [`store`]: the state store trait, the sealed snapshot's key and the local cache sealing.
//! * [`safety`]: safety numbers.

pub mod attachment;
pub mod engine;
pub mod envelope;
pub mod error;
pub mod groups;
pub mod identity;
pub mod ids;
pub mod messaging;
mod provider;
pub mod safety;
mod sealed;
pub mod store;
mod wire;

pub use engine::{Config, Engine, CIPHERSUITE};
pub use envelope::{AttachmentRef, Envelope, Kind};
pub use error::{Error, Result};
pub use ids::{ConversationId, DeviceId, DeviceRef, UserId};
pub use messaging::{Claimed, Incoming, Sealed};

/// Entry points the fuzz targets and property tests feed arbitrary bytes. Each must return, never
/// panic, whatever the input.
pub mod parsers {
    pub use crate::attachment::Header;

    /// The envelope decoder, for both kinds.
    pub fn envelope(bytes: &[u8]) {
        let _ = crate::Envelope::decode(crate::Kind::Message, bytes);
        let _ = crate::Envelope::decode(crate::Kind::Edit, bytes);
    }

    /// The sealed blob header.
    pub fn blob_header(bytes: &[u8]) {
        let _ = Header::parse(bytes);
    }

    /// Every MLS message shape the engine parses from the server: proposals, commits, Welcomes,
    /// ciphertexts, key packages, external sender keys, inside a fresh engine.
    pub fn mls_message(bytes: &[u8]) {
        crate::groups::fuzz_all(bytes);
    }

    /// The sealed attachment as a whole, opened with a fixed key.
    pub fn sealed_blob(bytes: &[u8]) {
        let key = crate::attachment::AttachmentKey::from_bytes([7u8; 32]);
        let _ = crate::attachment::open(&key, b"", bytes, std::io::sink(), crate::attachment::Expect::default());
    }
}
