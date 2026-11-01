//! One authenticated-encrypted blob, for the state snapshot and for the local message cache.
//!
//! `0x01 || salt(16) || nonce(12) || AES-256-GCM(ciphertext || tag)`. The AES key is
//! HKDF-SHA256(salt = the blob's random salt, ikm = the caller's 32 byte key, info = a label that
//! names what the key is for), so a key kept for years never meets a nonce twice and a key for one
//! purpose cannot open another's blobs. The caller's associated data (a store name, a row id) is
//! the GCM associated data.

use crate::error::{Error, Result};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

fn cipher(key: &[u8; 32], label: &[u8], salt: &[u8]) -> Result<Aes256Gcm> {
    let mut sub = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(salt), key).expand(label, &mut *sub).map_err(|_| Error::Authentication)?;
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&*sub)))
}

pub(crate) fn seal(key: &[u8; 32], label: &[u8], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| Error::Random)?;
    getrandom::fill(&mut nonce).map_err(|_| Error::Random)?;
    let sealed = cipher(key, label, &salt)?
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| Error::Authentication)?;
    let mut out = Vec::with_capacity(1 + SALT_LEN + NONCE_LEN + sealed.len());
    out.push(VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub(crate) fn open(key: &[u8; 32], label: &[u8], aad: &[u8], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if blob.len() < 1 + SALT_LEN + NONCE_LEN + TAG_LEN {
        return Err(Error::Malformed("sealed blob"));
    }
    if blob[0] != VERSION {
        return Err(Error::UnsupportedVersion);
    }
    let salt = &blob[1..1 + SALT_LEN];
    let nonce = &blob[1 + SALT_LEN..1 + SALT_LEN + NONCE_LEN];
    let body = &blob[1 + SALT_LEN + NONCE_LEN..];
    cipher(key, label, salt)?
        .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad })
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}
