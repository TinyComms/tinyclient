//! The encrypted backup file: the person's history as an [`crate::archive`], sealed under a key
//! that comes from a passphrase, saved wherever they choose and imported on a fresh device. No
//! server is involved, ever.
//!
//! ```text
//! "TNYB1" || salt (16) || memory KiB (u32 BE) || passes (u32 BE) || lanes (u32 BE) || archive
//! ```
//!
//! The key is Argon2id (RFC 9106, version 0x13) over the passphrase with the header's salt and
//! cost, 32 bytes. The whole header is the archive's `link_id`, so it is in every chunk's
//! associated data: a cost or salt changed in the file makes it fail to open rather than open
//! weaker. Reading a file takes at most [`MAX_MEMORY_KIB`] and [`MAX_PASSES`], so a crafted file
//! cannot make a device spend unbounded memory or time before it fails.

use crate::archive::ArchiveKey;
use crate::error::{Error, Result};
use argon2::{Algorithm, Argon2, Version};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 5] = b"TNYB1";
const SALT_LEN: usize = 16;
pub const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + 12;
/// What a file may ask of a device that reads it.
pub const MAX_MEMORY_KIB: u32 = 1024 * 1024;
pub const MAX_PASSES: u32 = 10;
pub const MAX_LANES: u32 = 8;

/// The cost of turning a passphrase into the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

impl Default for Params {
    /// RFC 9106's second recommended option (64 MiB, 3 passes), on one lane: about a second on
    /// a laptop, which is paid once per export or import.
    fn default() -> Params {
        Params {
            memory_kib: 64 * 1024,
            passes: 3,
            lanes: 1,
        }
    }
}

/// A backup's header: its salt and cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub salt: [u8; SALT_LEN],
    pub params: Params,
}

impl Header {
    /// A fresh random salt with `params`.
    pub fn new(params: Params) -> Result<Header> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|_| Error::Random)?;
        Ok(Header { salt, params })
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..5].copy_from_slice(MAGIC);
        out[5..21].copy_from_slice(&self.salt);
        out[21..25].copy_from_slice(&self.params.memory_kib.to_be_bytes());
        out[25..29].copy_from_slice(&self.params.passes.to_be_bytes());
        out[29..33].copy_from_slice(&self.params.lanes.to_be_bytes());
        out
    }

    /// The first [`HEADER_LEN`] bytes of a file. A file that asks for more than [`MAX_MEMORY_KIB`],
    /// [`MAX_PASSES`] or [`MAX_LANES`], or for less than Argon2 allows, is refused. Never panics.
    pub fn parse(bytes: &[u8]) -> Result<Header> {
        if bytes.len() < HEADER_LEN || &bytes[..5] != MAGIC {
            return Err(Error::Malformed("backup file"));
        }
        let word = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let params = Params {
            memory_kib: word(21),
            passes: word(25),
            lanes: word(29),
        };
        let fair = (1..=MAX_LANES).contains(&params.lanes)
            && (1..=MAX_PASSES).contains(&params.passes)
            && (8 * params.lanes..=MAX_MEMORY_KIB).contains(&params.memory_kib);
        if !fair {
            return Err(Error::Malformed("backup cost"));
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&bytes[5..21]);
        Ok(Header { salt, params })
    }

    /// The archive key for `passphrase`. A wrong passphrase gives another key, which opens
    /// nothing: the archive's first chunk fails.
    pub fn key(&self, passphrase: &str) -> Result<ArchiveKey> {
        let params = argon2::Params::new(
            self.params.memory_kib,
            self.params.passes,
            self.params.lanes,
            Some(32),
        )
        .map_err(|_| Error::Malformed("backup cost"))?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &self.salt, &mut *key)
            .map_err(|_| Error::Malformed("backup cost"))?;
        Ok(ArchiveKey::from_bytes(*key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> Params {
        Params {
            memory_kib: 64,
            passes: 1,
            lanes: 1,
        }
    }

    #[test]
    fn a_header_reads_back_and_one_passphrase_gives_one_key() {
        let header = Header::new(cheap()).unwrap();
        let back = Header::parse(&header.to_bytes()).unwrap();
        assert_eq!(back, header);
        let key = back.key("correct horse battery staple").unwrap();
        assert_eq!(
            key.as_bytes(),
            header
                .key("correct horse battery staple")
                .unwrap()
                .as_bytes()
        );
        assert_ne!(
            key.as_bytes(),
            header
                .key("correct horse battery stapler")
                .unwrap()
                .as_bytes()
        );
        let other = Header::new(cheap()).unwrap();
        assert_ne!(
            key.as_bytes(),
            other
                .key("correct horse battery staple")
                .unwrap()
                .as_bytes(),
            "a new salt"
        );
    }

    #[test]
    fn argon2id_is_the_rfc_9106_one() {
        // RFC 9106 section 5.3: Argon2id v0x13, m=32 KiB, t=3, p=4, with a secret and associated
        // data. The key here uses neither; this pins the algorithm and version underneath it.
        let params = argon2::ParamsBuilder::new()
            .m_cost(32)
            .t_cost(3)
            .p_cost(4)
            .data(argon2::AssociatedData::new(&[4u8; 12]).unwrap())
            .output_len(32)
            .build()
            .unwrap();
        let mut tag = [0u8; 32];
        Argon2::new_with_secret(&[3u8; 8], Algorithm::Argon2id, Version::V0x13, params)
            .unwrap()
            .hash_password_into(&[1u8; 32], &[2u8; 16], &mut tag)
            .unwrap();
        let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
        );
    }

    #[test]
    fn a_file_that_asks_too_much_or_too_little_is_refused() {
        let mut header = Header::new(Params::default()).unwrap();
        for params in [
            Params {
                memory_kib: MAX_MEMORY_KIB + 1,
                ..Params::default()
            },
            Params {
                passes: 0,
                ..Params::default()
            },
            Params {
                passes: MAX_PASSES + 1,
                ..Params::default()
            },
            Params {
                lanes: 0,
                ..Params::default()
            },
            Params {
                lanes: 9,
                ..Params::default()
            },
            Params {
                memory_kib: 7,
                lanes: 1,
                passes: 1,
            },
        ] {
            header.params = params;
            assert!(Header::parse(&header.to_bytes()).is_err(), "{params:?}");
        }
        assert!(Header::parse(b"TNYH1").is_err());
        assert!(Header::parse(&[0u8; HEADER_LEN]).is_err());
    }
}
