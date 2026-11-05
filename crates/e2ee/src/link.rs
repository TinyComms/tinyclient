//! Linking a new device of a person from one they already have: the link code, the channel the two
//! devices talk over through the server's link relay, and the messages they send on it
//! (tinyserver `docs/e2ee-text.md` section 5; the client contract in tiny's
//! `research_notes/device-link-protocol.md`).
//!
//! ```text
//! code = "tiny-link-1." || base64url( link_id (16) || new_pub (65) || secret (16) )
//! ```
//!
//! The NEW device makes a fresh P-256 key `nk` and 128 random bits `secret`, and shows the code as
//! text and as a QR of the same string. The person carries it to the OLD device, which makes its
//! own fresh key `ok` and posts `old_pub` through the relay in the clear. Both then compute
//!
//! ```text
//! z   = ECDH(nk, old_pub) = ECDH(ok, new_pub)
//! okm = HKDF-SHA256(salt = secret, ikm = z,
//!                   info = "tiny-link-v1" || link_id || new_pub || old_pub, L = 67)
//! k_new_to_old = okm[0..32], k_old_to_new = okm[32..64], check = okm[64..67]
//! ```
//!
//! The code is the out-of-band authenticator: the server sees `link_id` and `old_pub` but never the
//! code, so it cannot know `new_pub` or `secret`, and a key it puts in place of `old_pub` gives it
//! nothing it can open or forge. The six check digits ([`Channel::check_digits`]) are
//! `check` as a big-endian number mod 10^6; the person compares them on both screens, which also
//! catches a code that was swapped on its way.
//!
//! A channel message is `counter (u64, big endian) || AES-256-GCM(plaintext)`, nonce
//! `0x00000000 || counter`, associated data `link_id || direction || counter`, direction 0 from the
//! new device and 1 from the old. Each direction has its own key and counter; a message whose
//! counter is not above the last one opened is refused (a replay, a reorder, a reflection).

use crate::error::{Error, Result};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

/// What every link code starts with.
pub const CODE_PREFIX: &str = "tiny-link-1.";
pub const LINK_ID_LEN: usize = 16;
/// An uncompressed P-256 point.
pub const PUBLIC_LEN: usize = 65;
pub const SECRET_LEN: usize = 16;
const CODE_LEN: usize = LINK_ID_LEN + PUBLIC_LEN + SECRET_LEN;
const INFO: &[u8] = b"tiny-link-v1";
const COUNTER_LEN: usize = 8;
const TAG_LEN: usize = 16;
/// The relay takes 8 KiB a message; this is what a plaintext may be so that its message fits.
pub const MAX_PLAINTEXT: usize = 8 * 1024 - COUNTER_LEN - TAG_LEN;

/// What the new device shows. `secret` is zeroed when dropped and never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkCode {
    pub link_id: [u8; LINK_ID_LEN],
    pub new_pub: [u8; PUBLIC_LEN],
    secret: [u8; SECRET_LEN],
}

impl Drop for LinkCode {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl std::fmt::Debug for LinkCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkCode(..)")
    }
}

impl LinkCode {
    /// The code as text: what the QR draws and what is pasted.
    pub fn encode(&self) -> Zeroizing<String> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(CODE_LEN));
        bytes.extend_from_slice(&self.link_id);
        bytes.extend_from_slice(&self.new_pub);
        bytes.extend_from_slice(&self.secret);
        Zeroizing::new(format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(&*bytes)))
    }

    /// A pasted or scanned code. Whitespace anywhere is ignored (a code wrapped over lines);
    /// anything else that is not exactly a code, or whose key is not a point of the curve, is
    /// refused. Never panics.
    pub fn parse(text: &str) -> Result<LinkCode> {
        let compact: Zeroizing<String> =
            Zeroizing::new(text.chars().filter(|c| !c.is_whitespace()).collect());
        let body = compact
            .strip_prefix(CODE_PREFIX)
            .ok_or(Error::Malformed("link code"))?;
        if body.len() > 4 * CODE_LEN {
            return Err(Error::Malformed("link code"));
        }
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(body)
                .map_err(|_| Error::Malformed("link code"))?,
        );
        if bytes.len() != CODE_LEN {
            return Err(Error::Malformed("link code"));
        }
        let mut code = LinkCode {
            link_id: [0; LINK_ID_LEN],
            new_pub: [0; PUBLIC_LEN],
            secret: [0; SECRET_LEN],
        };
        code.link_id.copy_from_slice(&bytes[..LINK_ID_LEN]);
        code.new_pub
            .copy_from_slice(&bytes[LINK_ID_LEN..LINK_ID_LEN + PUBLIC_LEN]);
        code.secret
            .copy_from_slice(&bytes[LINK_ID_LEN + PUBLIC_LEN..]);
        public_key(&code.new_pub)?;
        Ok(code)
    }

    /// The link id as the relay names it (`/v1/e2ee/links/{id}`).
    pub fn link_id_text(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.link_id)
    }
}

/// A link id the relay gave, in its base64url form.
pub fn parse_link_id(text: &str) -> Result<[u8; LINK_ID_LEN]> {
    URL_SAFE_NO_PAD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| <[u8; LINK_ID_LEN]>::try_from(bytes).ok())
        .ok_or(Error::Malformed("link id"))
}

fn public_key(bytes: &[u8]) -> Result<PublicKey> {
    if bytes.len() != PUBLIC_LEN || bytes[0] != 4 {
        return Err(Error::Malformed("link key"));
    }
    PublicKey::from_sec1_bytes(bytes).map_err(|_| Error::Malformed("link key"))
}

fn fresh_key() -> Result<SecretKey> {
    loop {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *bytes).map_err(|_| Error::Random)?;
        // Zero or at least the group order: about 2^-32. Draw again.
        if let Ok(key) = SecretKey::from_slice(&*bytes) {
            return Ok(key);
        }
    }
}

fn encoded(key: &SecretKey) -> [u8; PUBLIC_LEN] {
    let point = key.public_key().to_encoded_point(false);
    let mut out = [0u8; PUBLIC_LEN];
    out.copy_from_slice(point.as_bytes());
    out
}

/// Which device a channel belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    New,
    Old,
}

impl Role {
    fn direction(self) -> u8 {
        match self {
            Role::New => 0,
            Role::Old => 1,
        }
    }

    /// The `from` label the relay carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::New => "new",
            Role::Old => "old",
        }
    }
}

/// The new device's half before the old device has answered. Holds the ephemeral key; dropped (and
/// zeroed) once [`Pending::accept`] makes the channel.
pub struct Pending {
    code: LinkCode,
    key: SecretKey,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending(..)")
    }
}

impl Pending {
    /// A fresh key and secret for the link the relay made as `link_id`.
    pub fn create(link_id: [u8; LINK_ID_LEN]) -> Result<Pending> {
        let key = fresh_key()?;
        let mut secret = [0u8; SECRET_LEN];
        getrandom::fill(&mut secret).map_err(|_| Error::Random)?;
        Ok(Pending {
            code: LinkCode {
                link_id,
                new_pub: encoded(&key),
                secret,
            },
            key,
        })
    }

    pub fn code(&self) -> &LinkCode {
        &self.code
    }

    /// The old device's key, from the relay: the channel. A key that is not a point is refused; a
    /// key the server put in its place opens a channel whose messages never authenticate and whose
    /// check digits do not match.
    pub fn accept(&self, old_pub: &[u8]) -> Result<Channel> {
        let theirs = public_key(old_pub)?;
        let mut old = [0u8; PUBLIC_LEN];
        old.copy_from_slice(old_pub);
        Channel::derive(Role::New, &self.code, &self.key, &theirs, &old)
    }
}

/// The old device's half, from the code the person brought: what to post (`old_pub`), and the
/// channel.
pub fn join(code: &LinkCode) -> Result<([u8; PUBLIC_LEN], Channel)> {
    let key = fresh_key()?;
    let old_pub = encoded(&key);
    let theirs = public_key(&code.new_pub)?;
    let channel = Channel::derive(Role::Old, code, &key, &theirs, &old_pub)?;
    Ok((old_pub, channel))
}

/// The authenticated channel between the two devices. Keys are zeroed when it is dropped.
pub struct Channel {
    role: Role,
    link_id: [u8; LINK_ID_LEN],
    send: Aes256Gcm,
    receive: Aes256Gcm,
    sent: u64,
    /// The lowest counter that may still be opened.
    next_in: u64,
    check: String,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl Channel {
    fn derive(
        role: Role,
        code: &LinkCode,
        mine: &SecretKey,
        theirs: &PublicKey,
        old_pub: &[u8; PUBLIC_LEN],
    ) -> Result<Channel> {
        let shared = p256::ecdh::diffie_hellman(mine.to_nonzero_scalar(), theirs.as_affine());
        let mut info = Vec::with_capacity(INFO.len() + LINK_ID_LEN + 2 * PUBLIC_LEN);
        info.extend_from_slice(INFO);
        info.extend_from_slice(&code.link_id);
        info.extend_from_slice(&code.new_pub);
        info.extend_from_slice(old_pub);
        let mut okm = Zeroizing::new([0u8; 67]);
        Hkdf::<Sha256>::new(Some(&code.secret), shared.raw_secret_bytes())
            .expand(&info, &mut *okm)
            .map_err(|_| Error::Authentication)?;
        let new_to_old = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&okm[..32]));
        let old_to_new = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&okm[32..64]));
        let n = u32::from_be_bytes([0, okm[64], okm[65], okm[66]]) % 1_000_000;
        let (send, receive) = match role {
            Role::New => (new_to_old, old_to_new),
            Role::Old => (old_to_new, new_to_old),
        };
        Ok(Channel {
            role,
            link_id: code.link_id,
            send,
            receive,
            sent: 0,
            next_in: 0,
            check: format!("{n:06}"),
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// The six digits both screens show.
    pub fn check_digits(&self) -> &str {
        &self.check
    }

    fn aad(&self, direction: u8, counter: u64) -> [u8; LINK_ID_LEN + 1 + COUNTER_LEN] {
        let mut aad = [0u8; LINK_ID_LEN + 1 + COUNTER_LEN];
        aad[..LINK_ID_LEN].copy_from_slice(&self.link_id);
        aad[LINK_ID_LEN] = direction;
        aad[LINK_ID_LEN + 1..].copy_from_slice(&counter.to_be_bytes());
        aad
    }

    fn nonce(counter: u64) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    /// One message to the other device.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(Error::TooLarge("link message"));
        }
        let counter = self.sent;
        self.sent = counter.checked_add(1).ok_or(Error::TooLarge("link"))?;
        let aad = self.aad(self.role.direction(), counter);
        let sealed = self
            .send
            .encrypt(
                Nonce::from_slice(&Self::nonce(counter)),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::Authentication)?;
        let mut out = Vec::with_capacity(COUNTER_LEN + sealed.len());
        out.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// One message from the other device. Refused, and nothing changes, unless it authenticates
    /// under the other direction's key with a counter above every one opened so far.
    pub fn open(&mut self, message: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if message.len() < COUNTER_LEN + TAG_LEN {
            return Err(Error::Malformed("link message"));
        }
        let mut counter = [0u8; COUNTER_LEN];
        counter.copy_from_slice(&message[..COUNTER_LEN]);
        let counter = u64::from_be_bytes(counter);
        if counter < self.next_in {
            return Err(Error::Authentication);
        }
        let other = match self.role {
            Role::New => Role::Old,
            Role::Old => Role::New,
        };
        let aad = self.aad(other.direction(), counter);
        let plain = self
            .receive
            .decrypt(
                Nonce::from_slice(&Self::nonce(counter)),
                Payload {
                    msg: &message[COUNTER_LEN..],
                    aad: &aad,
                },
            )
            .map_err(|_| Error::Authentication)?;
        self.next_in = counter.checked_add(1).ok_or(Error::Authentication)?;
        Ok(Zeroizing::new(plain))
    }

    pub fn send(&mut self, message: &Message) -> Result<Vec<u8>> {
        let plain = Zeroizing::new(
            serde_json::to_vec(message).map_err(|_| Error::Malformed("link message"))?,
        );
        self.seal(&plain)
    }

    pub fn receive(&mut self, sealed: &[u8]) -> Result<Message> {
        let plain = self.open(sealed)?;
        serde_json::from_slice(&plain).map_err(|_| Error::Malformed("link message"))
    }
}

/// How much history the old device sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    None,
    /// Messages of the last this many days.
    Days(u32),
    All,
}

impl Scope {
    pub fn parse(text: &str) -> Result<Scope> {
        match text {
            "none" => Ok(Scope::None),
            "all" => Ok(Scope::All),
            _ => text
                .strip_prefix("days:")
                .filter(|days| !days.is_empty() && days.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|days| days.parse::<u32>().ok())
                .filter(|days| *days > 0)
                .map(Scope::Days)
                .ok_or(Error::Malformed("history scope")),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::None => f.write_str("none"),
            Scope::Days(days) => write!(f, "days:{days}"),
            Scope::All => f.write_str("all"),
        }
    }
}

impl Serialize for Scope {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Scope, D::Error> {
        let text = String::deserialize(deserializer)?;
        Scope::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Where the history went: the archive's key and what the uploaded blob must be.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRef {
    /// Base64 of the 32 byte archive key.
    pub key: String,
    pub bytes: u64,
    /// Base64 of SHA-256 of the uploaded ciphertext.
    pub sha256: String,
    pub chunks: u64,
}

impl std::fmt::Debug for ArchiveRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveRef")
            .field("bytes", &self.bytes)
            .field("chunks", &self.chunks)
            .finish_non_exhaustive()
    }
}

/// What the two devices say on the channel, in order: `hello` (new to old), `approve` (old to new,
/// after the archive is uploaded), `done` (new to old). `abort` from either at any point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    Hello {
        device_id: String,
        /// Base64 of the new device's 65 byte signing key.
        signature_key: String,
        label: String,
        os: String,
    },
    Approve {
        /// Base64 of the old device's cross-signature over the new one.
        cross_signature: String,
        history: Scope,
        archive: Option<ArchiveRef>,
    },
    Done {},
    Abort {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Pending, Channel, Channel) {
        let pending = Pending::create([7u8; 16]).unwrap();
        let code = LinkCode::parse(&pending.code().encode()).unwrap();
        let (old_pub, old) = join(&code).unwrap();
        let new = pending.accept(&old_pub).unwrap();
        (pending, new, old)
    }

    #[test]
    fn both_sides_agree_on_the_check_and_talk_both_ways() {
        let (_, mut new, mut old) = pair();
        assert_eq!(new.check_digits(), old.check_digits());
        assert_eq!(new.check_digits().len(), 6);
        let hello = new.seal(b"hello").unwrap();
        assert_eq!(&*old.open(&hello).unwrap(), b"hello");
        let back = old.seal(b"approve").unwrap();
        assert_eq!(&*new.open(&back).unwrap(), b"approve");
    }

    #[test]
    fn a_message_reflected_back_to_its_sender_does_not_open() {
        let (_, mut new, _) = pair();
        let mine = new.seal(b"hello").unwrap();
        assert!(new.open(&mine).is_err());
    }

    #[test]
    fn scopes_read_back() {
        for scope in [Scope::None, Scope::All, Scope::Days(30)] {
            assert_eq!(Scope::parse(&scope.to_string()).unwrap(), scope);
        }
        for bad in [
            "days:",
            "days:0",
            "days:-1",
            "days:+3",
            "some",
            "ALL",
            "days:99999999999",
        ] {
            assert!(Scope::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn messages_are_the_contracts_json() {
        let hello = Message::Hello {
            device_id: "d".into(),
            signature_key: "k".into(),
            label: "Phone".into(),
            os: "Android".into(),
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert!(text.contains(r#""t":"hello""#), "{text}");
        let approve: Message = serde_json::from_str(
            r#"{"t":"approve","cross_signature":"c","history":"days:7","archive":null}"#,
        )
        .unwrap();
        assert_eq!(
            approve,
            Message::Approve {
                cross_signature: "c".into(),
                history: Scope::Days(7),
                archive: None
            }
        );
        let done: Message = serde_json::from_str(r#"{"t":"done"}"#).unwrap();
        assert_eq!(done, Message::Done {});
    }
}
