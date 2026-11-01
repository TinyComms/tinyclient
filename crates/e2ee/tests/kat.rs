//! Known-answer tests for the exact primitive the crate uses: AES-256-GCM (`aes_gcm::Aes256Gcm`,
//! 96-bit nonce, 128-bit tag), with the official NIST CAVP and Wycheproof vectors in `vectors.rs`.
//! The attachment blob, the state snapshot and the message cache all go through this type.

mod vectors;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use vectors::{Kat, NIST_DECRYPT, NIST_ENCRYPT, WYCHEPROOF};

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn run(kat: &Kat) {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&hex(kat.key)));
    let (iv, aad, pt) = (hex(kat.iv), hex(kat.aad), hex(kat.pt));
    assert_eq!(iv.len(), 12, "{}", kat.id);
    let mut sealed = hex(kat.ct);
    sealed.extend_from_slice(&hex(kat.tag));
    let opened = cipher.decrypt(
        GenericArray::from_slice(&iv),
        Payload {
            msg: &sealed,
            aad: &aad,
        },
    );
    if kat.valid {
        assert_eq!(
            opened.unwrap_or_else(|_| panic!("{}: valid vector did not decrypt", kat.id)),
            pt,
            "{}",
            kat.id
        );
        let encrypted = cipher
            .encrypt(
                GenericArray::from_slice(&iv),
                Payload {
                    msg: &pt,
                    aad: &aad,
                },
            )
            .unwrap();
        assert_eq!(
            encrypted, sealed,
            "{}: encryption differs from the vector",
            kat.id
        );
    } else {
        assert!(opened.is_err(), "{}: an invalid vector decrypted", kat.id);
    }
}

#[test]
fn nist_cavp_gcm_encrypt_vectors() {
    assert!(NIST_ENCRYPT.len() >= 10);
    for kat in NIST_ENCRYPT {
        run(kat);
    }
}

#[test]
fn nist_cavp_gcm_decrypt_vectors_including_the_failures() {
    assert!(NIST_DECRYPT.iter().any(|kat| kat.valid) && NIST_DECRYPT.iter().any(|kat| !kat.valid));
    for kat in NIST_DECRYPT {
        run(kat);
    }
}

#[test]
fn wycheproof_aes_gcm_vectors_valid_and_invalid() {
    assert!(WYCHEPROOF.len() >= 10);
    assert!(WYCHEPROOF.iter().any(|kat| kat.valid) && WYCHEPROOF.iter().any(|kat| !kat.valid));
    for kat in WYCHEPROOF {
        run(kat);
    }
}

#[test]
fn how_many_vectors_ran() {
    eprintln!(
        "NIST encrypt {}, NIST decrypt {}, Wycheproof {}",
        NIST_ENCRYPT.len(),
        NIST_DECRYPT.len(),
        WYCHEPROOF.len()
    );
    assert!(NIST_ENCRYPT.len() + NIST_DECRYPT.len() + WYCHEPROOF.len() >= 10);
}
