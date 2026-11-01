mod streams;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use e2ee::attachment::{self, AttachmentKey, Expect, Header, CHUNK_LOG2, HEADER_LEN};
use e2ee::Error;
use sha2::{Digest, Sha256};
use streams::*;

const CHUNK: usize = 1 << CHUNK_LOG2;
const SEALED_CHUNK: usize = CHUNK + 16;

fn key() -> AttachmentKey {
    AttachmentKey::from_bytes([0x42; 32])
}

fn seal_vec(key: &AttachmentKey, context: &[u8], plain: &[u8]) -> (Vec<u8>, attachment::SealInfo) {
    let mut out = Vec::new();
    let info = attachment::seal(key, context, plain, &mut out).unwrap();
    (out, info)
}

fn open_vec(
    len: usize,
    key: &AttachmentKey,
    context: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    attachment::open(key, context, sealed, &mut out, Expect::length(len as u64)).map(|_| out)
}

#[test]
fn round_trip_at_the_boundary_sizes() {
    for len in [
        0usize,
        1,
        15,
        16,
        17,
        CHUNK - 1,
        CHUNK,
        CHUNK + 1,
        2 * CHUNK - 1,
        2 * CHUNK,
        2 * CHUNK + 1,
        1 << 20,
    ] {
        let plain = pattern_bytes(len as u64 + 1, len);
        let (sealed, info) = seal_vec(&key(), b"att-1", &plain);
        assert_eq!(info.plain_len, len as u64, "plain length {len}");
        assert_eq!(info.sealed_len, sealed.len() as u64, "sealed length {len}");
        assert_eq!(
            attachment::sealed_len(len as u64),
            Some(sealed.len() as u64),
            "sealed_len({len})"
        );
        assert_eq!(info.padded_len, attachment::padded_len(len as u64).unwrap());
        assert_eq!(
            attachment::padded_len_of(sealed.len() as u64),
            Some(info.padded_len),
            "padded_len_of({len})"
        );
        assert_eq!(info.sha256, <[u8; 32]>::from(Sha256::digest(&sealed)));
        assert_eq!(&sealed[..5], b"TNYA1");
        assert_eq!(sealed[5], 16);
        let header = Header::parse(&sealed).unwrap();
        assert_eq!(header.chunk_log2, 16);
        let opened = open_vec(plain.len(), &key(), b"att-1", &sealed)
            .unwrap_or_else(|err| panic!("{len}: {err}"));
        assert_eq!(opened, plain, "{len}");
    }
}

#[test]
fn ten_mebibytes_round_trip_through_a_hashing_sink() {
    let len = 10u64 << 20;
    let mut sealed = Vec::with_capacity(len as usize + 1024);
    let info = attachment::seal(&key(), b"big", Pattern::new(7, len), &mut sealed).unwrap();
    assert_eq!(info.plain_len, len);
    let mut sink = HashSink::default();
    let opened = attachment::open(
        &key(),
        b"big",
        sealed.as_slice(),
        &mut sink,
        Expect {
            sha256: Some(&info.sha256),
            plain_len: len,
        },
    )
    .unwrap();
    assert_eq!(opened.plain_len, len);
    assert_eq!(sink.written, len);
    let mut expected = HashSink::default();
    std::io::copy(&mut Pattern::new(7, len), &mut expected).unwrap();
    assert_eq!(sink.finish(), expected.finish());
}

#[test]
fn sealed_len_and_padded_len_agree_everywhere_near_a_chunk() {
    for plain in (0u64..300)
        .chain((CHUNK as u64 - 20)..(CHUNK as u64 + 20))
        .chain((2 * CHUNK as u64 - 20)..(2 * CHUNK as u64 + 20))
    {
        let sealed = attachment::sealed_len(plain).unwrap();
        assert_eq!(
            attachment::padded_len_of(sealed),
            attachment::padded_len(plain),
            "{plain}"
        );
    }
    // Sizes no padded file seals to: too short for a tag, or holding a length Padme never makes.
    for sealed in [0u64, 12, 13, 14, 28, 29, 13 + 16 + 9, 13 + 16 + 257] {
        assert_eq!(attachment::padded_len_of(sealed), None, "{sealed}");
    }
}

#[test]
fn the_blob_is_exactly_aes_256_gcm_with_the_stream_nonce_layout() {
    // Rebuild a two-chunk blob by hand with the AES-256-GCM primitive alone, and compare. Pins the
    // construction: nonce = prefix(7) || counter(4, big endian) || last flag(1); the first chunk's
    // associated data is the header then the context; the rest have none.
    let key_bytes = [0x11u8; 32];
    let prefix = [1u8, 2, 3, 4, 5, 6, 7];
    let context = b"the-attachment-id";
    let plain = pattern_bytes(9, CHUNK + 100);
    // What is sealed is the file and its padding, 0x80 then zeros: CHUNK + 100 bytes, plus one,
    // round up to 67584.
    let mut padded = plain.clone();
    padded.push(0x80);
    padded.resize(67584, 0);
    let header = Header {
        chunk_log2: 16,
        nonce_prefix: prefix,
    };
    let mut sealed = Vec::new();
    attachment::seal_with_header(
        &AttachmentKey::from_bytes(key_bytes),
        context,
        header,
        plain.as_slice(),
        &mut sealed,
    )
    .unwrap();

    let cipher = Aes256Gcm::new(GenericArray::from_slice(&key_bytes));
    let nonce = |counter: u32, last: u8| {
        let mut nonce = [0u8; 12];
        nonce[..7].copy_from_slice(&prefix);
        nonce[7..11].copy_from_slice(&counter.to_be_bytes());
        nonce[11] = last;
        nonce
    };
    let mut aad = header.to_bytes().to_vec();
    aad.extend_from_slice(context);
    let first = cipher
        .encrypt(
            GenericArray::from_slice(&nonce(0, 0)),
            Payload {
                msg: &padded[..CHUNK],
                aad: &aad,
            },
        )
        .unwrap();
    let second = cipher
        .encrypt(
            GenericArray::from_slice(&nonce(1, 1)),
            Payload {
                msg: &padded[CHUNK..],
                aad: b"",
            },
        )
        .unwrap();
    let mut expected = header.to_bytes().to_vec();
    expected.extend_from_slice(&first);
    expected.extend_from_slice(&second);
    assert_eq!(sealed, expected);

    // And a single chunk is the last one from the start (40 bytes and the mark pad to 44).
    let one = pattern_bytes(3, 40);
    let mut one_padded = one.clone();
    one_padded.extend_from_slice(&[0x80, 0, 0, 0]);
    let mut sealed = Vec::new();
    attachment::seal_with_header(
        &AttachmentKey::from_bytes(key_bytes),
        b"",
        header,
        one.as_slice(),
        &mut sealed,
    )
    .unwrap();
    let only = cipher
        .encrypt(
            GenericArray::from_slice(&nonce(0, 1)),
            Payload {
                msg: &one_padded,
                aad: &header.to_bytes(),
            },
        )
        .unwrap();
    assert_eq!(&sealed[HEADER_LEN..], only.as_slice());

    // An empty file is one last chunk holding only the padding's mark.
    let mut sealed = Vec::new();
    attachment::seal_with_header(
        &AttachmentKey::from_bytes(key_bytes),
        b"",
        header,
        std::io::empty(),
        &mut sealed,
    )
    .unwrap();
    assert_eq!(sealed.len(), HEADER_LEN + 1 + 16);
}

#[test]
fn a_cut_blob_fails_wherever_it_is_cut() {
    let plain = pattern_bytes(1, 2 * CHUNK + 5000);
    let (sealed, _) = seal_vec(&key(), b"ctx", &plain);
    let mut cuts = vec![
        0,
        1,
        5,
        12,
        13,
        14,
        20,
        13 + 15,
        13 + 16,
        13 + SEALED_CHUNK - 1,
        13 + SEALED_CHUNK,
        13 + SEALED_CHUNK + 1,
        13 + 2 * SEALED_CHUNK,
        13 + 2 * SEALED_CHUNK + 15,
        sealed.len() - 17,
        sealed.len() - 16,
        sealed.len() - 1,
    ];
    cuts.sort_unstable();
    cuts.dedup();
    for cut in cuts {
        assert!(
            open_vec(plain.len(), &key(), b"ctx", &sealed[..cut]).is_err(),
            "a blob cut at {cut} of {} opened",
            sealed.len()
        );
    }
    // A blob of exact chunks cut at the last boundary (the file looks complete but is not). One
    // byte short of three chunks, so that the file and its mark fill them exactly.
    let plain = pattern_bytes(2, 3 * CHUNK - 1);
    let (sealed, info) = seal_vec(&key(), b"", &plain);
    assert_eq!(info.padded_len, 3 * CHUNK as u64);
    let at_boundary = 13 + 2 * SEALED_CHUNK;
    assert!(open_vec(plain.len(), &key(), b"", &sealed[..at_boundary]).is_err());
    assert!(open_vec(
        plain.len(),
        &key(),
        b"",
        &sealed[..at_boundary + SEALED_CHUNK]
    )
    .is_ok());
}

#[test]
fn chunks_reordered_repeated_dropped_or_appended_fail() {
    let plain = pattern_bytes(3, 4 * CHUNK + 10);
    let (sealed, _) = seal_vec(&key(), b"", &plain);
    let chunk = |n: usize| -> &[u8] {
        let start = 13 + n * SEALED_CHUNK;
        &sealed[start..(start + SEALED_CHUNK).min(sealed.len())]
    };
    let header = &sealed[..13];
    let build = |order: &[usize]| -> Vec<u8> {
        let mut out = header.to_vec();
        for n in order {
            out.extend_from_slice(chunk(*n));
        }
        out
    };
    // Whole, as a control.
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 2, 3, 4])).is_ok());
    // Reordered.
    assert!(open_vec(plain.len(), &key(), b"", &build(&[1, 0, 2, 3, 4])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 2, 1, 3, 4])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 2, 4, 3])).is_err());
    // A chunk repeated.
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 1, 2, 3, 4])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 0, 1, 2, 3, 4])).is_err());
    // A chunk dropped from the middle, from the front, from the end.
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 3, 4])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[1, 2, 3, 4])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 2, 3])).is_err());
    // The last chunk moved into the middle, or repeated at the end.
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 4, 2, 3])).is_err());
    assert!(open_vec(plain.len(), &key(), b"", &build(&[0, 1, 2, 3, 4, 4])).is_err());
    // Bytes appended.
    let mut longer = sealed.clone();
    longer.push(0);
    assert!(open_vec(plain.len(), &key(), b"", &longer).is_err());
    longer.extend_from_slice(&[0u8; 100]);
    assert!(open_vec(plain.len(), &key(), b"", &longer).is_err());
}

#[test]
fn any_flipped_bit_fails() {
    let plain = pattern_bytes(4, CHUNK + 300);
    let (sealed, _) = seal_vec(&key(), b"ctx", &plain);
    let spots = [
        0,
        4,
        5,
        6,
        12,
        13,
        14,
        100,
        13 + CHUNK,
        13 + SEALED_CHUNK - 1,
        13 + SEALED_CHUNK,
        sealed.len() / 2 + 1,
        sealed.len() - 17,
        sealed.len() - 1,
    ];
    for spot in spots {
        for bit in [0u8, 3, 7] {
            let mut bad = sealed.clone();
            bad[spot] ^= 1 << bit;
            assert!(
                open_vec(plain.len(), &key(), b"ctx", &bad).is_err(),
                "bit {bit} of byte {spot} was accepted"
            );
        }
    }
    // Header faults are told apart from authentication faults, not required to be.
    let mut bad = sealed.clone();
    bad[0] ^= 1;
    assert!(matches!(
        open_vec(plain.len(), &key(), b"ctx", &bad),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn the_wrong_key_or_the_wrong_associated_data_fails() {
    for len in [0usize, 1, CHUNK, CHUNK + 1, 3 * CHUNK + 7] {
        let plain = pattern_bytes(5, len);
        let (sealed, _) = seal_vec(&key(), b"id-1", &plain);
        assert!(open_vec(plain.len(), &key(), b"id-1", &sealed).is_ok());
        assert!(
            matches!(
                open_vec(
                    plain.len(),
                    &AttachmentKey::from_bytes([0x43; 32]),
                    b"id-1",
                    &sealed
                ),
                Err(Error::Authentication)
            ),
            "wrong key, {len}"
        );
        assert!(
            matches!(
                open_vec(plain.len(), &key(), b"id-2", &sealed),
                Err(Error::Authentication)
            ),
            "wrong context, {len}"
        );
        assert!(
            matches!(
                open_vec(plain.len(), &key(), b"", &sealed),
                Err(Error::Authentication)
            ),
            "no context, {len}"
        );
        assert!(
            matches!(
                open_vec(plain.len(), &key(), b"id-1x", &sealed),
                Err(Error::Authentication)
            ),
            "longer context, {len}"
        );
    }
}

#[test]
fn the_header_binds_its_own_chunk_size_and_prefix() {
    let plain = pattern_bytes(6, 3 * CHUNK);
    let (sealed, _) = seal_vec(&key(), b"", &plain);
    // The chunk size byte changed to another allowed value.
    let mut other = sealed.clone();
    other[5] = 15;
    assert!(open_vec(plain.len(), &key(), b"", &other).is_err());
    // A chunk size that is not allowed at all is refused before anything is allocated for it.
    for size in [0u8, 9, 21, 255] {
        let mut bad = sealed.clone();
        bad[5] = size;
        assert!(
            matches!(
                open_vec(plain.len(), &key(), b"", &bad),
                Err(Error::Malformed(_))
            ),
            "{size}"
        );
    }
}

#[test]
fn the_expected_hash_and_length_are_checked_in_constant_time_after_the_last_chunk() {
    let plain = pattern_bytes(7, CHUNK + 1);
    let (sealed, info) = seal_vec(&key(), b"", &plain);
    let mut out = Vec::new();
    attachment::open(
        &key(),
        b"",
        sealed.as_slice(),
        &mut out,
        Expect {
            sha256: Some(&info.sha256),
            plain_len: plain.len() as u64,
        },
    )
    .unwrap();
    let mut wrong = info.sha256;
    wrong[31] ^= 1;
    assert!(attachment::open(
        &key(),
        b"",
        sealed.as_slice(),
        std::io::sink(),
        Expect {
            sha256: Some(&wrong),
            plain_len: plain.len() as u64
        }
    )
    .is_err());
    assert!(attachment::open(
        &key(),
        b"",
        sealed.as_slice(),
        std::io::sink(),
        Expect {
            sha256: None,
            plain_len: plain.len() as u64 + 1
        }
    )
    .is_err());
}

#[test]
fn open_verified_checks_the_hash_before_a_byte_is_decrypted() {
    struct Forbidden;
    impl std::io::Write for Forbidden {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            panic!("plaintext reached the writer before the hash was checked");
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let plain = pattern_bytes(8, 2 * CHUNK);
    let (sealed, info) = seal_vec(&key(), b"", &plain);
    let mut wrong = info.sha256;
    wrong[0] ^= 0x80;
    let result = attachment::open_verified(
        &key(),
        b"",
        std::io::Cursor::new(&sealed),
        Forbidden,
        &wrong,
        plain.len() as u64,
    );
    assert!(matches!(result, Err(Error::Authentication)));
    // The right hash opens it, from a reader that has been read before.
    let mut cursor = std::io::Cursor::new(&sealed);
    std::io::copy(&mut cursor, &mut std::io::sink()).unwrap();
    let mut out = Vec::new();
    attachment::open_verified(
        &key(),
        b"",
        cursor,
        &mut out,
        &info.sha256,
        plain.len() as u64,
    )
    .unwrap();
    assert_eq!(out, plain);
}

#[test]
fn a_decrypted_file_is_private_and_a_failed_one_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let plain = pattern_bytes(9, 3 * CHUNK + 11);
    let (sealed, info) = seal_vec(&key(), b"id", &plain);

    // Good: the temp file exists, holds the plaintext, is private, and is kept only by persist.
    let temp = attachment::decrypt_to_temp(
        &key(),
        b"id",
        sealed.as_slice(),
        dir.path(),
        Expect {
            sha256: Some(&info.sha256),
            plain_len: plain.len() as u64,
        },
    )
    .unwrap();
    let temp_path = temp.path().to_path_buf();
    assert_eq!(std::fs::read(&temp_path).unwrap(), plain);
    assert!(temp_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(attachment::TEMP_PREFIX));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&temp_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(temp);
    assert!(
        !temp_path.exists(),
        "dropping the temp file did not remove it"
    );

    let temp = attachment::decrypt_to_temp(
        &key(),
        b"id",
        sealed.as_slice(),
        dir.path(),
        Expect::length(plain.len() as u64),
    )
    .unwrap();
    let kept = dir.path().join("photo.bin");
    temp.persist(&kept).unwrap();
    assert_eq!(std::fs::read(&kept).unwrap(), plain);

    // Bad: every kind of failure leaves no file behind, though plaintext had been written.
    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    let cut = &sealed[..13 + SEALED_CHUNK + 5];
    let before: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    for (name, blob, context) in [
        ("tampered last chunk", tampered.as_slice(), b"id".as_slice()),
        ("cut", cut, b"id"),
        ("wrong context", sealed.as_slice(), b"nope"),
    ] {
        assert!(
            attachment::decrypt_to_temp(
                &key(),
                context,
                blob,
                dir.path(),
                Expect::length(plain.len() as u64)
            )
            .is_err(),
            "{name}"
        );
        let after: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before, after, "{name} left a file behind");
    }
    // A wrong expected hash is a failure after all the data is written: still nothing left.
    assert!(attachment::decrypt_to_temp(
        &key(),
        b"id",
        sealed.as_slice(),
        dir.path(),
        Expect {
            sha256: Some(&[0u8; 32]),
            plain_len: plain.len() as u64
        }
    )
    .is_err());
    let after: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn stale_temp_files_from_a_killed_process_are_swept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".tnya-leftover"), b"partial plaintext").unwrap();
    std::fs::write(dir.path().join("keep.txt"), b"mine").unwrap();
    assert_eq!(
        attachment::remove_stale_temps(dir.path(), std::time::Duration::from_secs(3600)).unwrap(),
        0
    );
    assert_eq!(
        attachment::remove_stale_temps(dir.path(), std::time::Duration::ZERO).unwrap(),
        1
    );
    assert!(!dir.path().join(".tnya-leftover").exists());
    assert!(dir.path().join("keep.txt").exists());
}

#[test]
fn a_writer_that_fails_stops_the_open_and_a_reader_that_fails_stops_the_seal() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("unplugged"))
        }
    }
    let plain = pattern_bytes(10, 1000);
    let (sealed, _) = seal_vec(&key(), b"", &plain);
    assert!(matches!(
        attachment::open(&key(), b"", sealed.as_slice(), Broken, Expect::length(1000)),
        Err(Error::Io(_))
    ));
    assert!(matches!(
        attachment::seal(&key(), b"", Broken, Vec::new()),
        Err(Error::Io(_))
    ));
}

#[test]
fn keys_are_random_unique_redacted_and_compared_without_leaking() {
    let a = AttachmentKey::generate().unwrap();
    let b = AttachmentKey::generate().unwrap();
    assert_ne!(a, b);
    assert_eq!(a, a.clone());
    assert_eq!(format!("{a:?}"), "AttachmentKey(..)");
    // Two seals of the same file under the same key differ (a fresh nonce prefix each time).
    let plain = pattern_bytes(11, 100);
    let (one, _) = seal_vec(&a, b"", &plain);
    let (two, _) = seal_vec(&a, b"", &plain);
    assert_ne!(one, two);
    assert_ne!(
        Header::parse(&one).unwrap().nonce_prefix,
        Header::parse(&two).unwrap().nonce_prefix
    );
}

#[test]
fn padme_gives_the_published_lengths() {
    // Padme, Nikitin et al., PETS 2019, Algorithm 1: E = floor(log2 L), S = floor(log2 E) + 1,
    // the low E - S bits rounded up. Worked by hand from the algorithm, e.g. L = 100: E = 6,
    // S = 3, 3 low bits rounded up: 104. L = 1,000,000: E = 19, S = 5, 14 bits: 62 * 16384.
    for (len, padded) in [
        (0u64, 0u64),
        (1, 1),
        (2, 2),
        (3, 3),
        (7, 7),
        (8, 8),
        (9, 10),
        (11, 12),
        (15, 16),
        (17, 18),
        (100, 104),
        (129, 144),
        (255, 256),
        (256, 256),
        (257, 272),
        (1000, 1024),
        (1025, 1088),
        (65536, 65536),
        (65537, 67584),
        (100_000, 100_352),
        (1_000_000, 1_015_808),
        (3_200_000, 3_211_264),
        (1_000_000_000, 1_006_632_960),
        (1 << 40, 1 << 40),
        ((1 << 40) + 1, (1 << 40) + (1 << 34)),
    ] {
        assert_eq!(attachment::padme(len), Some(padded), "padme({len})");
    }
    assert_eq!(attachment::padme(u64::MAX), None);
}

#[test]
fn files_of_nearby_sizes_seal_to_the_same_size() {
    // Every length from 65,536 to 67,583 pads to 67,584: the server sees one size for all.
    let sizes: std::collections::BTreeSet<u64> = (65_536u64..67_584)
        .step_by(97)
        .chain([65_536, 67_583])
        .map(|len| attachment::sealed_len(len).unwrap())
        .collect();
    assert_eq!(sizes.len(), 1, "{sizes:?}");
    // Sealed for real, two files a thousand bytes apart are the same number of bytes, and each
    // opens to exactly itself.
    let (small, large) = (pattern_bytes(11, 65_600), pattern_bytes(12, 66_600));
    let (small_sealed, small_info) = seal_vec(&key(), b"a", &small);
    let (large_sealed, large_info) = seal_vec(&key(), b"a", &large);
    assert_eq!(small_sealed.len(), large_sealed.len());
    assert_eq!(small_info.padded_len, 67_584);
    assert_eq!(large_info.padded_len, 67_584);
    assert_eq!(
        (small_info.plain_len, large_info.plain_len),
        (65_600, 66_600)
    );
    assert_eq!(
        open_vec(small.len(), &key(), b"a", &small_sealed).unwrap(),
        small
    );
    assert_eq!(
        open_vec(large.len(), &key(), b"a", &large_sealed).unwrap(),
        large
    );
    // Across a whole range, the number of distinct sealed sizes is a small fraction of the lengths.
    let distinct: std::collections::BTreeSet<u64> = (1_000_000u64..1_100_000)
        .map(|len| attachment::sealed_len(len).unwrap())
        .collect();
    assert!(distinct.len() <= 8, "{} sizes", distinct.len());
}

#[test]
fn opening_needs_the_true_length_and_refuses_a_wrong_one() {
    let cases = [
        0usize,
        1,
        9,
        100,
        1000,
        CHUNK - 1,
        CHUNK + 1,
        2 * CHUNK + 5000,
    ]
    .into_iter()
    .flat_map(|len| {
        // Any data, data that is all zeros, and data that ends in 0x80 and zeros: the
        // padding's mark tells where the file ends whatever its last bytes are.
        let mut tricky = vec![0u8; len];
        if len > 3 {
            tricky[len - 3] = 0x80;
        }
        [pattern_bytes(len as u64 + 3, len), vec![0u8; len], tricky]
    });
    for plain in cases {
        let len = plain.len();
        let (sealed, info) = seal_vec(&key(), b"len", &plain);
        let padded = attachment::padded_len(len as u64).unwrap();
        assert_eq!(info.padded_len, padded);
        // The padding is in the blob: it is the padded length that is sealed.
        assert_eq!(
            sealed.len() as u64,
            attachment::sealed_len(len as u64).unwrap()
        );
        let mut out = Vec::new();
        let opened = attachment::open(
            &key(),
            b"len",
            sealed.as_slice(),
            &mut out,
            Expect {
                sha256: Some(&info.sha256),
                plain_len: len as u64,
            },
        )
        .unwrap();
        assert_eq!(out, plain, "{len}: not exactly the file");
        assert_eq!(opened.plain_len, len as u64);

        // Every other length is refused: shorter (the padding would hold file bytes), longer
        // (past the file, or another padded size), or one that pads past the blob.
        let mut wrong: Vec<u64> = vec![len as u64 + 1, padded + 1, padded * 2 + 2];
        if len > 0 {
            wrong.push(len as u64 - 1);
            wrong.push(0);
        }
        if padded > len as u64 {
            wrong.push(padded);
        }
        for claimed in wrong {
            if claimed == len as u64 {
                continue;
            }
            assert!(
                matches!(
                    attachment::open(
                        &key(),
                        b"len",
                        sealed.as_slice(),
                        std::io::sink(),
                        Expect::length(claimed)
                    ),
                    Err(Error::Authentication)
                ),
                "{len}: claimed {claimed} was accepted"
            );
        }
    }
}
