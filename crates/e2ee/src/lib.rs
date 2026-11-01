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
//!
//! # The calls, in the order a client makes them
//!
//! 1. Once per install: [`Engine::create`] (or [`Engine::open`] after that). Register
//!    `engine.device_id()` and `engine.signature_key()` with `PUT /v1/e2ee/devices/{id}`; a second
//!    device of a person gets its `cross_signature` from a trusted one: [`Engine::cross_sign`].
//!    Upload [`Engine::key_packages`] (up to 50) and one [`Engine::last_resort_key_package`]; top
//!    up on `e2ee.keys_low`; replace the last-resort one within 84 days.
//!    Every device list the server shows (members of a conversation, the person themself) goes
//!    through [`Engine::verify_devices`] first. Nothing is added to a group that was not.
//!    `GET /v1/e2ee/conversations/{id}`: [`Engine::observe_server_epoch`] and
//!    [`Engine::observe_server_key`] with its `epoch` and `external_sender`.
//! 2. The device the server tells to start the group: [`Engine::create_group`].
//! 3. The handshake feed, in order, each item to its handler: `proposals` to
//!    [`Engine::handle_proposals`], `commit` to [`Engine::handle_commit`], `welcome` to
//!    [`Engine::handle_welcome`]. Proposals pending: [`Engine::commit_pending`], POST the
//!    [`groups::CommitBundle`], then [`Engine::confirm_commit`] on `204`. On `409` or a lost reply
//!    read the feed: the `commit` item settles it ([`Engine::pending_commit`] has ours to send
//!    again). [`error::Error::UntrustedDevices`] means: verify those people's device lists and
//!    call again.
//! 4. Messages: [`Engine::encrypt`] (`Kind::Message`, or `Kind::Edit` with the target id) and
//!    [`Engine::decrypt`] with what the row says ([`Claimed`]); an edit counts only after
//!    [`Incoming::authorize_edit`] with the stored author of its target. Messages and commits of
//!    one epoch: open the messages of epoch `e` before applying the commit that leaves `e`, or
//!    within [`Config::max_past_epochs`] after it. A device cannot open its own messages: keep the
//!    plaintext when you send.
//! 5. Attachments: [`attachment::seal`] a file into the upload (new [`attachment::AttachmentKey`]
//!    each), put key, `sha256` and sizes in the [`envelope::AttachmentRef`]; download into a file,
//!    then [`attachment::decrypt_to_temp`] (or [`attachment::open_verified`]), and `persist` the
//!    temp file only if it returned `Ok`.
//! 6. The local cache of decrypted messages: [`store::seal_cache_value`] with the message id as
//!    `row_id`.

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
        let _ = crate::attachment::open(
            &key,
            b"",
            bytes,
            std::io::sink(),
            crate::attachment::Expect::default(),
        );
    }
}
