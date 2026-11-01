//! Small helpers shared by the state snapshot's serde types.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Bytes that serialize as one base64 string, so a snapshot of keys and MLS state is not a JSON
/// array of numbers.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        STANDARD
            .decode(text.as_bytes())
            .map(Bytes)
            .map_err(serde::de::Error::custom)
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({})", self.0.len())
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Bytes {
        Bytes(bytes)
    }
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

/// A MLS variable-length vector's contents, from the front of `bytes`: the prefix (1, 2 or 4
/// bytes by the top two bits of the first) and the bytes it announces. Returns the contents and
/// what follows. Never panics.
pub(crate) fn read_vl(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = *bytes.first()?;
    let (len, header): (usize, usize) = match first >> 6 {
        0 => (usize::from(first & 0x3f), 1),
        1 => (
            usize::from(first & 0x3f) << 8 | usize::from(*bytes.get(1)?),
            2,
        ),
        2 => {
            let rest = bytes.get(1..4)?;
            (
                usize::from(first & 0x3f) << 24
                    | usize::from(rest[0]) << 16
                    | usize::from(rest[1]) << 8
                    | usize::from(rest[2]),
                4,
            )
        }
        _ => return None,
    };
    let end = header.checked_add(len)?;
    Some((bytes.get(header..end)?, bytes.get(end..)?))
}

/// Runs a parser or a library call on bytes from the network, and turns a panic into an error.
///
/// Two of the libraries below assert on malformed input (tls_codec on a bad length prefix, openmls
/// with a `debug_assert!` on a failed AEAD check in debug builds). A hostile server must not be
/// able to crash the client with those, so every call that takes untrusted bytes goes through
/// here. A panic in the middle of a group operation may leave that group half changed in memory:
/// callers reload it.
pub(crate) fn guard<T>(f: impl FnOnce() -> T) -> crate::error::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|_| crate::error::Error::Malformed("input"))
}
