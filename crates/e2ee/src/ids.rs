//! Identifiers: all three are 16 bytes on the wire (a UUID), and a MLS credential is exactly two
//! of them.

use crate::error::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Length of the credential of every leaf: a user id then a device id.
pub const CREDENTIAL_LEN: usize = 32;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub [u8; 16]);

        impl $name {
            /// Parses a UUID, hyphenated or not, in either case.
            pub fn parse(text: &str) -> Result<$name> {
                parse_uuid(text).map($name).ok_or(Error::Malformed("id"))
            }

            pub fn from_slice(bytes: &[u8]) -> Result<$name> {
                <[u8; 16]>::try_from(bytes).map($name).map_err(|_| Error::Malformed("id"))
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// A fresh random id (UUID version 4 bits set, as the server's ids are).
            pub fn random() -> Result<$name> {
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                Ok($name(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_uuid(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "("))?;
                write_uuid(&self.0, f)?;
                write!(f, ")")
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<$name, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                $name::parse(&text).map_err(serde::de::Error::custom)
            }
        }
    };
}

id_type!(
    /// An account.
    UserId
);
id_type!(
    /// One install of the client signed in to one account.
    DeviceId
);
id_type!(
    /// A conversation, and the MLS group id of its group.
    ConversationId
);

fn write_uuid(bytes: &[u8; 16], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for (at, byte) in bytes.iter().enumerate() {
        if matches!(at, 4 | 6 | 8 | 10) {
            f.write_str("-")?;
        }
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    if text.len() > 36 || !text.is_ascii() {
        return None;
    }
    let digits: Vec<u8> = text.bytes().filter(|byte| *byte != b'-').collect();
    if digits.len() != 32 {
        return None;
    }
    // Hyphens only where a UUID has them.
    if text.len() == 36 && [8, 13, 18, 23].iter().any(|at| text.as_bytes()[*at] != b'-') {
        return None;
    }
    if text.len() != 36 && text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (at, pair) in digits.chunks(2).enumerate() {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out[at] = (high * 16 + low) as u8;
    }
    Some(out)
}

/// A device in a group or on a person's list: whose it is, which one, and its signing key.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceRef {
    pub user: UserId,
    pub device: DeviceId,
    /// Uncompressed P-256, 65 bytes.
    pub signature_key: Vec<u8>,
}

impl fmt::Debug for DeviceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceRef({}/{})", self.user, self.device)
    }
}

/// The credential content of a leaf: `user_id || device_id`.
pub fn credential_bytes(user: UserId, device: DeviceId) -> [u8; CREDENTIAL_LEN] {
    let mut out = [0u8; CREDENTIAL_LEN];
    out[..16].copy_from_slice(&user.0);
    out[16..].copy_from_slice(&device.0);
    out
}

/// The reverse of [`credential_bytes`]; anything but exactly 32 bytes is refused.
pub fn parse_credential(bytes: &[u8]) -> Result<(UserId, DeviceId)> {
    if bytes.len() != CREDENTIAL_LEN {
        return Err(Error::Malformed("credential"));
    }
    Ok((UserId::from_slice(&bytes[..16])?, DeviceId::from_slice(&bytes[16..])?))
}
