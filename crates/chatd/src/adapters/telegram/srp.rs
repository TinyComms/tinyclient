//! Telegram's SRP, the proof of the cloud password (https://core.telegram.org/api/srp), in one
//! place. Signing in and transferring ownership prove a password the account already has
//! ([`proof`], through grammers' own arithmetic). Setting or changing one sends a new verifier
//! made from what the user typed ([`password_x`], [`verifier`]): grammers keeps both inside its
//! proof and gives nothing back, so they are written here, step for step the same. A verifier
//! that does not match the typed password would leave a password nobody knows on the account
//! (#542). The scripted wire (`fake`) checks a proof with the same helpers, as Telegram's server
//! does.
//!
//! Nothing here prints or keeps a password. The big-number arithmetic runs on a thread with a
//! 4 MiB stack: in a debug build num-bigint's modular powers use more than a connection thread
//! has.

use grammers_crypto::two_factor_auth::{calculate_2fa, check_p_and_g};
use grammers_tl_types as tl;
use num_bigint::BigUint;
use sha2::{Digest, Sha256, Sha512};

/// How many times the password is stretched before `x` comes out of it.
const ROUNDS: u32 = 100_000;
/// The stack the arithmetic gets.
const STACK: usize = 4 * 1024 * 1024;

/// H(data) := sha256(data), over `parts` in order.
pub(super) fn h(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    parts.iter().for_each(|part| hasher.update(part));
    hasher.finalize().into()
}

/// SH(data, salt) := H(salt | data | salt)
fn sh(data: &[u8], salt: &[u8]) -> [u8; 32] {
    h(&[salt, data, salt])
}

/// PH1(password, salt1, salt2) := SH(SH(password, salt1), salt2)
fn ph1(password: &[u8], salt1: &[u8], salt2: &[u8]) -> [u8; 32] {
    sh(&sh(password, salt1), salt2)
}

/// PH2(password, salt1, salt2) := SH(pbkdf2(sha512, PH1(password, salt1, salt2), salt1,
/// 100000), salt2): `x`, the password as a number. `salt1` is the whole one: the server's with
/// the client's 32 random bytes appended, exactly as the new password's `new_algo` carries it.
pub(super) fn password_x(password: &[u8], salt1: &[u8], salt2: &[u8]) -> [u8; 32] {
    let mut stretched = [0u8; 64];
    pbkdf2::pbkdf2_hmac::<Sha512>(&ph1(password, salt1, salt2), salt1, ROUNDS, &mut stretched);
    sh(&stretched, salt2)
}

/// `bytes` as Telegram writes a number: big-endian, left-padded with zeros to 256. Nothing longer
/// comes here: what arrives off the network is checked before it is padded.
pub(super) fn pad256(bytes: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    out[256 - bytes.len()..].copy_from_slice(bytes);
    out
}

/// v := g^x mod p
fn v(x: &[u8; 32], p: &[u8], g: i32) -> [u8; 256] {
    let modulus = BigUint::from_bytes_be(p);
    pad256(&BigUint::from_bytes_be(x).modpow(&BigUint::from(g as u32), &modulus).to_bytes_be())
}

/// The verifier of `x`: `g^x mod p`, 256 bytes big-endian, which is what
/// `account.updatePasswordSettings` takes as `new_password_hash`. `p` and `g` are checked with
/// [`trusted`] first, and `g` is one of Telegram's (2 to 7).
pub(super) fn verifier(x: &[u8; 32], p: &[u8], g: i32) -> Vec<u8> {
    match with_stack("srp-verifier", || v(x, p, g)) {
        Ok(v) => v.to_vec(),
        // The thread is for a debug build's stack. Without one the sum is the same where it stands.
        Err(_) => v(x, p, g).to_vec(),
    }
}

/// Whether `p` and `g` are Telegram's own and may be used. Checked before anything is proved or
/// made with them, because a bad group would give the password away. (`check_p_and_g` panics
/// on a generator outside 2 to 7.)
pub(super) fn trusted(p: &[u8], g: i32) -> bool {
    (2..=7).contains(&g) && check_p_and_g(p, &g)
}

/// Runs `work` on a thread with stack to spare. `Err` when the thread could not start or its
/// work panicked. A panic's own words are not passed on: they may say what was being computed.
pub(super) fn with_stack<T: Send>(name: &str, work: impl FnOnce() -> T + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(name.to_owned())
            .stack_size(STACK)
            .spawn_scoped(scope, work)
            .map_err(|err| format!("the SRP thread could not start: {err}"))?
            .join()
            .map_err(|_| "the SRP calculation failed".to_owned())
    })
}

/// Why no proof could be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Unproved {
    /// Nothing to prove: the account has no password, or keeps it in a way this build does not
    /// know.
    KeptOtherwise,
    /// Telegram's numbers cannot be trusted: a bad group would give the password away.
    Untrusted,
    /// The calculation could not run.
    Failed(String),
}

/// A proof of `password` for the numbers in `asked` (an `account.getPassword`): what
/// `auth.checkPassword` and `account.updatePasswordSettings` take as their `password`.
pub(super) fn proof(
    asked: &tl::types::account::Password,
    password: &str,
) -> Result<tl::enums::InputCheckPasswordSrp, Unproved> {
    let (Some(tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(algo)), Some(g_b), Some(srp_id)) =
        (&asked.current_algo, asked.srp_b.as_deref(), asked.srp_id)
    else {
        return Err(Unproved::KeptOtherwise);
    };
    // Nothing longer than Telegram's 256 bytes is made anything of: grammers pads without
    // looking, so a longer number would be a panic where a refusal belongs.
    if !trusted(&algo.p, algo.g) || g_b.len() > 256 || asked.secure_random.len() > 256 {
        return Err(Unproved::Untrusted);
    }
    let (m1, g_a) = with_stack("srp-proof", || {
        calculate_2fa(&algo.salt1, &algo.salt2, &algo.p, &algo.g, g_b.to_vec(), asked.secure_random.clone(), password.as_bytes())
    })
    .map_err(Unproved::Failed)?;
    Ok(tl::types::InputCheckPasswordSrp { srp_id, a: g_a.to_vec(), m1: m1.to_vec() }.into())
}

#[cfg(test)]
mod tests {
    use super::super::fake::{cloud, unhex, Fake, PRIME};
    use super::super::signin;
    use super::super::wire::Fault;
    use super::super::Saved;
    use super::*;
    use grammers_crypto::two_factor_auth::calculate_2fa;
    use grammers_tl_types as tl;
    use num_bigint::BigUint;

    /// `account.getPassword` as Telegram answers it for these numbers.
    fn asked(p: Vec<u8>, g: i32, salt1: Vec<u8>, salt2: Vec<u8>, srp_b: Option<Vec<u8>>, secure_random: Vec<u8>) -> tl::types::account::Password {
        let srp_id = srp_b.is_some().then_some(77);
        tl::types::account::Password {
            has_recovery: false,
            has_secure_values: false,
            has_password: srp_id.is_some(),
            current_algo: Some(tl::types::PasswordKdfAlgoSha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow { salt1, salt2, g, p }.into()),
            srp_b,
            srp_id,
            hint: None,
            email_unconfirmed_pattern: None,
            new_algo: tl::enums::PasswordKdfAlgo::Unknown,
            new_secure_algo: tl::enums::SecurePasswordKdfAlgo::Unknown,
            secure_random,
            pending_reset_date: None,
            login_email_pattern: None,
        }
    }

    /// Telegram's own numbers, the way the fake hands them out.
    fn real(p: Vec<u8>, g: i32) -> tl::types::account::Password {
        asked(p, g, vec![7; 8], vec![9; 16], Some(vec![5; 256]), vec![3; 256])
    }

    /// A proof the way a client makes it: grammers' own, for numbers the server issued.
    fn client(srp_b: &[u8], typed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (m1, a) = with_stack("a client's proof in a test", || {
            calculate_2fa(&[7; 8], &[9; 16], &unhex(PRIME), &3, srp_b.to_vec(), vec![3; 256], typed)
        })
        .expect("the SRP thread");
        (m1.to_vec(), a.to_vec())
    }

    #[test]
    fn a_verifier_of_ours_is_what_a_proof_of_the_same_password_checks_against() {
        let p = unhex(PRIME);
        for typed in ["correct horse", "0p3n s3sam3!", "gnu \u{1F980} chimney"] {
            // What a client would send to set `typed`: made here, with nothing of grammers'.
            let x = password_x(typed.as_bytes(), &[7; 8], &[9; 16]);
            let set = verifier(&x, &p, 3);
            assert_eq!(set.len(), 256, "2048 bits");
            let straight = BigUint::from_bytes_be(&x).modpow(&BigUint::from(3u32), &BigUint::from_bytes_be(&p)).to_bytes_be();
            assert_eq!(&set[256 - straight.len()..], straight.as_slice(), "big-endian, left-padded to 256 bytes");
            assert!(set[..256 - straight.len()].iter().all(|byte| *byte == 0));

            // Telegram's side issues the numbers of one attempt from that verifier, then checks
            // what the client proves against it.
            let attempt = cloud::issued(&set);
            let (m1, a) = client(&attempt.srp_b, typed.as_bytes());
            assert!(cloud::proves(&set, &[7; 8], &[9; 16], &attempt.secret, &attempt.srp_b, &a, &m1), "{typed}: the proof of the password itself was refused");
            // The verifier of another password refuses it: the check tells them apart.
            let another = verifier(&password_x(b"another one", &[7; 8], &[9; 16]), &p, 3);
            assert!(!cloud::proves(&another, &[7; 8], &[9; 16], &attempt.secret, &attempt.srp_b, &a, &m1), "{typed}: a proof was taken for another password");
            // And the proof of a password nobody set is refused for the right verifier.
            let (wrong, wrong_a) = client(&attempt.srp_b, b"another one");
            assert!(!cloud::proves(&set, &[7; 8], &[9; 16], &attempt.secret, &attempt.srp_b, &wrong_a, &wrong), "{typed}: another password was taken");
        }
    }

    #[test]
    fn grammers_small_vector_comes_out_of_our_proof() {
        // The vector at the end of grammers' `two_factor_auth.rs`: p = 47, g = 3, salt1 [1],
        // salt2 [2], g_b [5], a [6], password [7]. If our road to the proof or grammers itself
        // drifts, this is the first to say so.
        let asked = asked(pad256(&[47]).to_vec(), 3, vec![1], vec![2], Some(vec![5]), vec![6]);
        let tl::enums::InputCheckPasswordSrp::Srp(srp) = proof(&asked, "\u{7}").expect("a proof of the small vector") else {
            panic!("no proof was made");
        };
        assert_eq!(
            srp.m1,
            [157, 131, 196, 103, 0, 184, 116, 232, 7, 196, 85, 231, 17, 36, 30, 222, 158, 234, 98, 88, 59, 56, 71, 215, 183, 123, 122, 50, 19, 32, 54, 206]
        );
        assert_eq!(srp.a, pad256(&[24]), "3^6 mod 47");
        assert_eq!(srp.srp_id, 77);
    }

    #[test]
    fn nothing_is_proved_with_numbers_that_cannot_be_trusted() {
        let good = real(unhex(PRIME), 3);
        // No password to prove.
        let none = tl::types::account::Password { has_password: false, current_algo: None, srp_b: None, srp_id: None, ..good.clone() };
        assert_eq!(proof(&none, "hunter2"), Err(Unproved::KeptOtherwise));
        // A password kept in a way this build does not know.
        let unknown = tl::types::account::Password { current_algo: Some(tl::enums::PasswordKdfAlgo::Unknown), ..good.clone() };
        assert_eq!(proof(&unknown, "hunter2"), Err(Unproved::KeptOtherwise));

        // A generator outside Telegram's own.
        assert_eq!(proof(&real(unhex(PRIME), 1), "hunter2"), Err(Unproved::Untrusted));
        // A p that is no safe prime.
        assert_eq!(proof(&real(vec![0xff; 256], 3), "hunter2"), Err(Unproved::Untrusted));
        // A p of the wrong length.
        assert_eq!(proof(&real(vec![0xff; 128], 3), "hunter2"), Err(Unproved::Untrusted));
        // Numbers longer than Telegram's 256 bytes: nothing is made of them, and nothing panics.
        let long = asked(unhex(PRIME), 3, vec![7; 8], vec![9; 16], Some(vec![5; 300]), vec![3; 256]);
        assert_eq!(proof(&long, "hunter2"), Err(Unproved::Untrusted));
        let long = asked(unhex(PRIME), 3, vec![7; 8], vec![9; 16], Some(vec![5; 256]), vec![3; 300]);
        assert_eq!(proof(&long, "hunter2"), Err(Unproved::Untrusted));

        // The same numbers with a generator Telegram does use: a proof.
        let tl::enums::InputCheckPasswordSrp::Srp(srp) = proof(&good, "correct horse").expect("a proof") else {
            panic!("no proof was made");
        };
        assert_eq!((srp.m1.len(), srp.a.len(), srp.srp_id), (32, 256, 77));
    }

    /// Each step of the real prime, worked by a program that is neither grammers' nor ours
    /// (Python's `hashlib`, following https://core.telegram.org/api/srp as written): "correct
    /// horse" stretched with the fake's salts, the client's `a` of [3; 256], the server's `b` = 5.
    #[test]
    fn every_step_agrees_with_an_independent_worked_example() {
        let (p, salt1, salt2) = (unhex(PRIME), vec![7; 8], vec![9; 16]);
        // x, the password as a number.
        let x = password_x(b"correct horse", &salt1, &salt2);
        assert_eq!(
            x[..],
            unhex("77619718d00274f524ab5b57962443fbe5c38e6b32717a67ea8d30fa4309afae")[..],
            "x: the password stretched"
        );
        // v = g^x mod p.
        let v = unhex(
            "266695708e7465d3bebdebef6c23757a736b7d23f988242a8c651ff5ee896da5ca324cab5f4bc3c7cd6520c65d0ad8277a978c9641a03aa1cbf4cd0191a121af319475a883fd555b6a34182d0f9a11883e99484b27cb54c8187738d136a9b93482b670b45305eae6f2af977408038a8df8c1ce13d3c166a4dfdc81219b731135cca2ae8f898a5b38bf8a98f475b311e9e135912b0192771907f5a9319000ac80faefe21065f7ff57233b9ea06eacf163a0a4960484f5c593e40640ee4d8c902e01d85e3fdc17c8907b0a79d0b7fcc80f05dc6c2912eabf1fcbc7947873216024476499a99c3e3fe2d84e3c86df64b1c7e0a283c9b6e743c37bbfb863b5b300d5",
        );
        assert_eq!(verifier(&x, &p, 3), v, "v: the verifier");
        // What the server issues for b = 5, and what the client proves for it.
        let issued_b = unhex(
            "1c0875076797aa75b81bca96c48e95ac16741dd45dcad1c66076f182b83f37a5435599faa50c0d09d93d248577a3a86e89b3859d83f23178ef30072f7012e403e354bb83f28b0afa75d923738e633985d2467242504d1c17a2cf0a48122d74a5535a9fcd34330c595f8ab77bb90dccd1813ea1300c69fa893ba19d7dee8856688060aa20298c741cf67114c31a6c6f2a15f1f17f8da083237efc8183d89c358df23e059052cab288744897b5def5cf1bd6602d44452a2ba8dd5609e4e5f050d1de786e5b7ac1c762c53613f64b2f75b9a96a7805ac575839ef411cb34cdd63243361c79f05a89c28b5ea03a3c166eb22a000325eb0364e588c714a4a1764842e",
        );
        let (proved_m1, proved_a) = client(&issued_b, b"correct horse");
        assert_eq!(
            proved_a[..],
            unhex("3012b7383669bc58b6d284f97d204a0aca11d8ad9ba3e80f45ddeb2dac74801379f4ec9943454685ae8721340786b19de6065335d3fbc6bbc744b6e56dae471f12147a3359f282d3876a41a9c0d6c1ebcb4b8fe7440e61ccd69c1275947f43eddd2f6a51fed486bec012932cc4c9d4d0fa932cf27e46519fdd1cc2f226c388820dc4b6d305752e909aa248b4cecd4c36d99a29fdd5009dc6122bf65a4cbcf49b3baab15afb1a321b2dfe4dee607c64274ee59f96ff4cb60d1ccab3922231e1d56677f842620bf9eba451429c33081e9fe9d85fcbdb0d02b0c01f2df313e0f84d80a010ac8c5d932e05c06ba6886c30dfcda1dfa75d979495fca6ab6d4718e59f")[..],
            "A: grammers' own"
        );
        assert_eq!(
            proved_m1[..],
            unhex("6388b87ad9fbd72c917de1c2f2a6517e0c28643ef34cdd37ddb7d695e0ce2f60")[..],
            "M1: grammers' proof for that B"
        );
        // The server's side takes it, for that verifier and that secret.
        assert!(cloud::proves(&v, &salt1, &salt2, &pad256(&[5]), &issued_b, &proved_a, &proved_m1), "the server's check of a proof the example agrees with");
        // And what the fake issues for a random secret is checked the same way.
        let attempt = cloud::issued(&v);
        assert_eq!(attempt.srp_b.len(), 256, "B");
        let (m1, a) = client(&attempt.srp_b, b"correct horse");
        assert!(cloud::proves(&v, &salt1, &salt2, &attempt.secret, &attempt.srp_b, &a, &m1), "a proof against numbers the fake issued");
    }

    #[test]
    fn the_proof_the_ownership_transfer_sends_is_one_telegram_takes() {
        // `srp_proof` has no scripted wire of its own: the fake does not answer
        // `messages.editChatCreator`. So it is read against the fake's server side directly.
        let _one = cloud::one_at_a_time();
        let signed_in = Saved { user: 7_001, name: "Mira Kessler".to_owned(), home: 2, dcs: Vec::new() };
        let fake = Fake::alone(Some(&signed_in));
        let tl::enums::InputCheckPasswordSrp::Srp(srp) = signin::srp_proof(&fake, "correct horse").expect("a proof") else {
            panic!("no proof was made");
        };
        assert_eq!((srp.m1.len(), srp.a.len()), (32, 256));
        assert_eq!(cloud::check(7_001, &srp), Ok(()));
        // The proof of another password is refused, and names nothing of either.
        let tl::enums::InputCheckPasswordSrp::Srp(wrong) = signin::srp_proof(&fake, "hunter2").expect("a proof") else {
            panic!("no proof was made");
        };
        assert_eq!(cloud::check(7_001, &wrong), Err(Fault::Rpc { code: 400, name: "PASSWORD_HASH_INVALID".to_owned(), value: None }));
        // An account with no password has nothing to prove: that is what the transfer says too.
        fake.steer("two-step off");
        assert_eq!(signin::srp_proof(&fake, "correct horse"), Err(Fault::Rpc { code: 400, name: "PASSWORD_REQUIRED".to_owned(), value: None }));
    }
}
