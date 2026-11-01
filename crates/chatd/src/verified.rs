//! Whom the user verified in end-to-end encrypted Tiny calls, and where each account keeps its
//! identity key. Both live in the account's own directory under the support directory, never in
//! the store: the identity key is mediad's to read, and a verification is a fact about a key,
//! not about a conversation.
//!
//! A verification remembers the key it was made for. A person whose key is not that key any more
//! shows as `Trust::Changed`, which the call's lock says loudly: either they set tinyclient up
//! again, or someone (the server among them) is putting another key in their place.

use protocol::{AccountKey, CallSecurity, Trust};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// People remembered per account. A server has a few hundred at most.
const KEPT: usize = 4_096;
/// The longest key kept: P-256, uncompressed, in hex.
const KEY: usize = 130;

/// The account's directory for end-to-end encryption, under `base` (the support directory).
fn dir(base: &Path, account: &AccountKey) -> PathBuf {
    let safe: String = format!("{:?}-{}", account.network, account.account)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(120)
        .collect();
    base.join("e2ee").join(safe)
}

/// The support directory; a scratch one in chatd's own tests, which must never write to the
/// user's.
fn base() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("chatd-test-e2ee-{}", std::process::id()));
    }
    protocol::support_dir()
}

/// Where mediad keeps this account's identity key, made there on its first call.
pub fn identity_path(account: &AccountKey) -> PathBuf {
    dir(&base(), account).join("identity.key")
}

fn verified_path(base: &Path, account: &AccountKey) -> PathBuf {
    dir(base, account).join("verified.json")
}

/// User id to the key the user verified for them.
fn read(base: &Path, account: &AccountKey) -> BTreeMap<String, String> {
    std::fs::read(verified_path(base, account)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Remembers `user_id`'s `key` as verified, or forgets that they were.
pub fn set(account: &AccountKey, user_id: &str, key: &str, verified: bool) -> io::Result<()> {
    set_in(&base(), account, user_id, key, verified)
}

fn set_in(base: &Path, account: &AccountKey, user_id: &str, key: &str, verified: bool) -> io::Result<()> {
    let key = key.trim().to_ascii_lowercase();
    if user_id.is_empty() || key.is_empty() || key.len() > KEY || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a key"));
    }
    let mut known = read(base, account);
    if verified {
        if known.len() >= KEPT && !known.contains_key(user_id) {
            return Err(io::Error::other("too many people verified"));
        }
        known.insert(user_id.to_owned(), key);
    } else {
        known.remove(user_id);
    }
    let path = verified_path(base, account);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_vec_pretty(&known).map_err(io::Error::other)?;
    // Written beside and moved over, so a crash never leaves half a file.
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(partial, path)
}

/// mediad's report with each member's trust filled in from what the user verified.
pub fn trusted(account: &AccountKey, security: CallSecurity) -> CallSecurity {
    trusted_in(&base(), account, security)
}

fn trusted_in(base: &Path, account: &AccountKey, mut security: CallSecurity) -> CallSecurity {
    let known = read(base, account);
    for member in &mut security.members {
        member.trust = match known.get(&member.user_id) {
            Some(key) if key.eq_ignore_ascii_case(&member.key) => Trust::Verified,
            Some(_) => Trust::Changed,
            None => Trust::Unverified,
        };
    }
    security
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Network, SecureMember, SecurityMode};

    fn member(user: &str, key: &str) -> SecureMember {
        SecureMember { user_id: user.into(), key: key.into(), ..SecureMember::default() }
    }

    #[test]
    fn a_verified_key_is_verified_until_it_changes_and_forgetting_it_is_unverified() {
        let scratch = std::env::temp_dir().join(format!("chatd-verified-{}", std::process::id()));
        let account = AccountKey { network: Network::Tiny, account: "me/../x".into() };
        assert!(dir(&scratch, &account).parent() == Some(&scratch.join("e2ee")), "a hostile id stays inside");
        let (set, trusted) = (|user: &str, key: &str, on| set_in(&scratch, &account, user, key, on), |s| trusted_in(&scratch, &account, s));
        let security = CallSecurity { mode: SecurityMode::EndToEnd, members: vec![member("ada", "aa01"), member("bea", "bb02")], ..CallSecurity::default() };
        assert!(trusted(security.clone()).members.iter().all(|m| m.trust == Trust::Unverified));

        set("ada", "AA01", true).unwrap();
        let seen = trusted(security.clone());
        assert_eq!((seen.members[0].trust, seen.members[1].trust), (Trust::Verified, Trust::Unverified));

        let changed = CallSecurity { members: vec![member("ada", "cc03")], ..security.clone() };
        assert_eq!(trusted(changed).members[0].trust, Trust::Changed, "a new key under a verified name is loud");

        set("ada", "aa01", false).unwrap();
        assert_eq!(trusted(security).members[0].trust, Trust::Unverified);
        assert!(set("ada", "not hex", true).is_err());
        assert!(set("", "aa", true).is_err());
        let _ = std::fs::remove_dir_all(scratch);
    }
}
