//! Telegram 1:1 call end-to-end encryption verification emoji (333-emoji table).
//!
//! Matches `telegramdesktop/tdesktop` `calls_emoji_fingerprint.cpp` and Telegram Android
//! `EmojiHelper.java`: `SHA-256(key || g_a)` split into four 64-bit big-endian integers
//! with the sign bit cleared (`& 0x7FFF_FFFF_FFFF_FFFF`), each taken modulo 333.

use sha2::{Digest, Sha256};

/// Canonical Telegram 333-emoji table in exact official order.
pub const TELEGRAM_CALL_EMOJIS: [&str; 333] = [
    "\u{1f609}", "\u{1f60d}", "\u{1f61b}", "\u{1f62d}", "\u{1f631}", "\u{1f621}", "\u{1f60e}", "\u{1f634}", "\u{1f635}", "\u{1f608}", "\u{1f62c}", "\u{1f607}",
    "\u{1f60f}", "\u{1f46e}", "\u{1f477}", "\u{1f482}", "\u{1f476}", "\u{1f468}", "\u{1f469}", "\u{1f474}", "\u{1f475}", "\u{1f63b}", "\u{1f63d}", "\u{1f640}",
    "\u{1f47a}", "\u{1f648}", "\u{1f649}", "\u{1f64a}", "\u{1f480}", "\u{1f47d}", "\u{1f4a9}", "\u{1f525}", "\u{1f4a5}", "\u{1f4a4}", "\u{1f442}", "\u{1f440}",
    "\u{1f443}", "\u{1f445}", "\u{1f444}", "\u{1f44d}", "\u{1f44e}", "\u{1f44c}", "\u{1f44a}", "\u{270c}", "\u{270b}", "\u{1f450}", "\u{1f446}", "\u{1f447}",
    "\u{1f449}", "\u{1f448}", "\u{1f64f}", "\u{1f44f}", "\u{1f4aa}", "\u{1f6b6}", "\u{1f3c3}", "\u{1f483}", "\u{1f46b}", "\u{1f46a}", "\u{1f46c}", "\u{1f46d}",
    "\u{1f485}", "\u{1f3a9}", "\u{1f451}", "\u{1f452}", "\u{1f45f}", "\u{1f45e}", "\u{1f460}", "\u{1f455}", "\u{1f457}", "\u{1f456}", "\u{1f459}", "\u{1f45c}",
    "\u{1f453}", "\u{1f380}", "\u{1f484}", "\u{1f49b}", "\u{1f499}", "\u{1f49c}", "\u{1f49a}", "\u{1f48d}", "\u{1f48e}", "\u{1f436}", "\u{1f43a}", "\u{1f431}",
    "\u{1f42d}", "\u{1f439}", "\u{1f430}", "\u{1f438}", "\u{1f42f}", "\u{1f428}", "\u{1f43b}", "\u{1f437}", "\u{1f42e}", "\u{1f417}", "\u{1f434}", "\u{1f411}",
    "\u{1f418}", "\u{1f43c}", "\u{1f427}", "\u{1f425}", "\u{1f414}", "\u{1f40d}", "\u{1f422}", "\u{1f41b}", "\u{1f41d}", "\u{1f41c}", "\u{1f41e}", "\u{1f40c}",
    "\u{1f419}", "\u{1f41a}", "\u{1f41f}", "\u{1f42c}", "\u{1f40b}", "\u{1f410}", "\u{1f40a}", "\u{1f42b}", "\u{1f340}", "\u{1f339}", "\u{1f33b}", "\u{1f341}",
    "\u{1f33e}", "\u{1f344}", "\u{1f335}", "\u{1f334}", "\u{1f333}", "\u{1f31e}", "\u{1f31a}", "\u{1f319}", "\u{1f30e}", "\u{1f30b}", "\u{26a1}", "\u{2614}",
    "\u{2744}", "\u{26c4}", "\u{1f300}", "\u{1f308}", "\u{1f30a}", "\u{1f393}", "\u{1f386}", "\u{1f383}", "\u{1f47b}", "\u{1f385}", "\u{1f384}", "\u{1f381}",
    "\u{1f388}", "\u{1f52e}", "\u{1f3a5}", "\u{1f4f7}", "\u{1f4bf}", "\u{1f4bb}", "\u{260e}", "\u{1f4e1}", "\u{1f4fa}", "\u{1f4fb}", "\u{1f509}", "\u{1f514}",
    "\u{23f3}", "\u{23f0}", "\u{231a}", "\u{1f512}", "\u{1f511}", "\u{1f50e}", "\u{1f4a1}", "\u{1f526}", "\u{1f50c}", "\u{1f50b}", "\u{1f6bf}", "\u{1f6bd}",
    "\u{1f527}", "\u{1f528}", "\u{1f6aa}", "\u{1f6ac}", "\u{1f4a3}", "\u{1f52b}", "\u{1f52a}", "\u{1f48a}", "\u{1f489}", "\u{1f4b0}", "\u{1f4b5}", "\u{1f4b3}",
    "\u{2709}", "\u{1f4eb}", "\u{1f4e6}", "\u{1f4c5}", "\u{1f4c1}", "\u{2702}", "\u{1f4cc}", "\u{1f4ce}", "\u{2712}", "\u{270f}", "\u{1f4d0}", "\u{1f4da}",
    "\u{1f52c}", "\u{1f52d}", "\u{1f3a8}", "\u{1f3ac}", "\u{1f3a4}", "\u{1f3a7}", "\u{1f3b5}", "\u{1f3b9}", "\u{1f3bb}", "\u{1f3ba}", "\u{1f3b8}", "\u{1f47e}",
    "\u{1f3ae}", "\u{1f0cf}", "\u{1f3b2}", "\u{1f3af}", "\u{1f3c8}", "\u{1f3c0}", "\u{26bd}", "\u{26be}", "\u{1f3be}", "\u{1f3b1}", "\u{1f3c9}", "\u{1f3b3}",
    "\u{1f3c1}", "\u{1f3c7}", "\u{1f3c6}", "\u{1f3ca}", "\u{1f3c4}", "\u{2615}", "\u{1f37c}", "\u{1f37a}", "\u{1f377}", "\u{1f374}", "\u{1f355}", "\u{1f354}",
    "\u{1f35f}", "\u{1f357}", "\u{1f371}", "\u{1f35a}", "\u{1f35c}", "\u{1f361}", "\u{1f373}", "\u{1f35e}", "\u{1f369}", "\u{1f366}", "\u{1f382}", "\u{1f370}",
    "\u{1f36a}", "\u{1f36b}", "\u{1f36d}", "\u{1f36f}", "\u{1f34e}", "\u{1f34f}", "\u{1f34a}", "\u{1f34b}", "\u{1f352}", "\u{1f347}", "\u{1f349}", "\u{1f353}",
    "\u{1f351}", "\u{1f34c}", "\u{1f350}", "\u{1f34d}", "\u{1f346}", "\u{1f345}", "\u{1f33d}", "\u{1f3e1}", "\u{1f3e5}", "\u{1f3e6}", "\u{26ea}", "\u{1f3f0}",
    "\u{26fa}", "\u{1f3ed}", "\u{1f5fb}", "\u{1f5fd}", "\u{1f3a0}", "\u{1f3a1}", "\u{26f2}", "\u{1f3a2}", "\u{1f6a2}", "\u{1f6a4}", "\u{2693}", "\u{1f680}",
    "\u{2708}", "\u{1f681}", "\u{1f682}", "\u{1f68b}", "\u{1f68e}", "\u{1f68c}", "\u{1f699}", "\u{1f697}", "\u{1f695}", "\u{1f69b}", "\u{1f6a8}", "\u{1f694}",
    "\u{1f692}", "\u{1f691}", "\u{1f6b2}", "\u{1f6a0}", "\u{1f69c}", "\u{1f6a6}", "\u{26a0}", "\u{1f6a7}", "\u{26fd}", "\u{1f3b0}", "\u{1f5ff}", "\u{1f3aa}",
    "\u{1f3ad}", "\u{1f1ef}\u{1f1f5}", "\u{1f1f0}\u{1f1f7}", "\u{1f1e9}\u{1f1ea}", "\u{1f1e8}\u{1f1f3}", "\u{1f1fa}\u{1f1f8}", "\u{1f1eb}\u{1f1f7}", "\u{1f1ea}\u{1f1f8}", "\u{1f1ee}\u{1f1f9}", "\u{1f1f7}\u{1f1fa}", "\u{1f1ec}\u{1f1e7}", "\u{31}\u{20e3}",
    "\u{32}\u{20e3}", "\u{33}\u{20e3}", "\u{34}\u{20e3}", "\u{35}\u{20e3}", "\u{36}\u{20e3}", "\u{37}\u{20e3}", "\u{38}\u{20e3}", "\u{39}\u{20e3}", "\u{30}\u{20e3}", "\u{1f51f}", "\u{2757}", "\u{2753}",
    "\u{2665}", "\u{2666}", "\u{1f4af}", "\u{1f517}", "\u{1f531}", "\u{1f534}", "\u{1f535}", "\u{1f536}", "\u{1f537}",
];

/// Computes the four verification emoji from a 32-byte SHA-256 digest of `key || g_a`.
pub fn telegram_call_emojis_from_digest(digest: &[u8; 32]) -> [&'static str; 4] {
    let pick = |i: usize| -> &'static str {
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&digest[i * 8..(i + 1) * 8]);
        let val = u64::from_be_bytes(chunk) & 0x7FFF_FFFF_FFFF_FFFF;
        TELEGRAM_CALL_EMOJIS[(val % TELEGRAM_CALL_EMOJIS.len() as u64) as usize]
    };
    [pick(0), pick(1), pick(2), pick(3)]
}

/// Computes the four Telegram call verification emoji from the shared `key` and `g_a`.
/// Must only be shown after `phone.confirmCall` succeeds and the key fingerprint matches.
pub fn telegram_call_emojis(key: &[u8], g_a: &[u8]) -> [&'static str; 4] {
    let mut hasher = Sha256::new();
    hasher.update(key);
    hasher.update(g_a);
    let digest: [u8; 32] = hasher.finalize().into();
    telegram_call_emojis_from_digest(&digest)
}

/// Computes the four verification emoji for an encrypted Telegram conference call from its
/// verified E2EE chain state hash (`tde2e`-compatible 333-emoji mapping).
/// Recomputed on every membership change so all verified participants see matching emoji.
pub fn telegram_conference_emojis(state_hash: &[u8; 32]) -> [&'static str; 4] {
    let mut hasher = Sha256::new();
    hasher.update(b"tde2e-emoji-v1:");
    hasher.update(state_hash);
    let digest: [u8; 32] = hasher.finalize().into();
    telegram_call_emojis_from_digest(&digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_333_canonical_entries() {
        assert_eq!(TELEGRAM_CALL_EMOJIS.len(), 333);
        assert_eq!(TELEGRAM_CALL_EMOJIS[0], "\u{1f609}");
        assert_eq!(TELEGRAM_CALL_EMOJIS[332], "\u{1f537}");
    }

    #[test]
    fn computes_exact_official_vector_for_known_key_and_g_a() {
        let key = [0x01u8; 32];
        let g_a = [0x02u8; 32];
        let got = telegram_call_emojis(&key, &g_a);
        let expected = [TELEGRAM_CALL_EMOJIS[268], TELEGRAM_CALL_EMOJIS[330], TELEGRAM_CALL_EMOJIS[138], TELEGRAM_CALL_EMOJIS[121]];
        assert_eq!(got, expected);
    }

    #[test]
    fn clears_sign_bit_of_each_64_bit_chunk_matching_official_clients() {
        let mut digest = [0u8; 32];
        digest[0] = 0x80;
        digest[15] = 1;
        digest[22..24].copy_from_slice(&333u16.to_be_bytes());
        digest[30..32].copy_from_slice(&332u16.to_be_bytes());
        let got = telegram_call_emojis_from_digest(&digest);
        assert_eq!(got, [TELEGRAM_CALL_EMOJIS[0], TELEGRAM_CALL_EMOJIS[1], TELEGRAM_CALL_EMOJIS[0], TELEGRAM_CALL_EMOJIS[332]]);
    }

    #[test]
    fn telegram_verified_requires_confirmation_and_fingerprint_match() {
        let key = [0x01u8; 32];
        let g_a = [0x02u8; 32];
        assert!(crate::CallSecurity::telegram_verified(&key, &g_a, false, true).is_none());
        assert!(crate::CallSecurity::telegram_verified(&key, &g_a, true, false).is_none());
        assert!(crate::CallSecurity::telegram_verified(&key, &g_a, false, false).is_none());
        let sec = crate::CallSecurity::telegram_verified(&key, &g_a, true, true).expect("confirmed + matched");
        assert_eq!(sec.mode, crate::SecurityMode::EndToEnd);
        assert_eq!(sec.emojis.len(), 4);
        assert_eq!(sec.emojis, telegram_call_emojis(&key, &g_a));

        let hash_a = [0x11u8; 32];
        let hash_b = [0x22u8; 32];
        let conf_a = crate::CallSecurity::telegram_conference_verified(&hash_a, 1, Vec::new());
        let conf_b = crate::CallSecurity::telegram_conference_verified(&hash_b, 2, Vec::new());
        assert_eq!(conf_a.mode, crate::SecurityMode::EndToEnd);
        assert!(conf_a.required);
        assert_eq!(conf_a.emojis.len(), 4);
        assert_ne!(conf_a.emojis, conf_b.emojis);
        let blocked = crate::CallSecurity::telegram_conference_blocked();
        assert_eq!(blocked.mode, crate::SecurityMode::Blocked);
        assert!(blocked.required);
        assert!(blocked.emojis.is_empty());
    }
}
