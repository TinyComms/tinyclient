//! Signed-in sessions, kept in the login keychain and nowhere else.
//!
//! One item per network, holding what that network's sign-in produced. A sign-in that yields
//! several accounts (Slack workspaces) is still one item; `adapters::accounts` splits it.

use keyring::Entry;
use protocol::Network;

const SERVICE: &str = "tinyclient";

/// False when `TINYCHAT_DIR` points chatd at a scratch directory. Tests and the demo never touch
/// the keychain and never sign in.
pub fn enabled() -> bool {
    std::env::var_os("TINYCHAT_DIR").is_none()
}

/// Said instead of reading or saving when this build's keyring keeps nothing past the process
/// (its in-memory mock, chosen when no system store was compiled in) or past a reboot.
pub const NO_STORE: &str = "This copy of tinyclient has no keychain to keep sign-ins in, so it signs nobody in. Install a build for this system.";

/// Whether sign-ins are kept until deleted: the macOS keychain, the Windows credential store,
/// the Secret Service on Linux. Asked once.
pub fn persistent() -> bool {
    use keyring::credential::CredentialPersistence;
    static PERSISTENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PERSISTENT.get_or_init(|| {
        let persistence = keyring::default::default_credential_builder().persistence();
        let kept = matches!(persistence, CredentialPersistence::UntilDelete);
        if !kept {
            eprintln!("chatd: the keyring does not keep sign-ins past a restart; refusing to read or save any");
        }
        kept
    })
}

fn account(network: Network) -> &'static str {
    network.label()
}

/// The session JSON the sign-in produced, if there is one. In a scratch directory the only
/// session is one a test hands over in `TINYCHAT_SESSION_DISCORD` or `TINYCHAT_SESSION_SLACK`,
/// which it points at a local fake server with the variables in `adapters::net`.
pub fn load(network: Network) -> Option<String> {
    match read(network) {
        Stored::Found(session) => Some(session),
        Stored::Absent | Stored::Unreadable(_) => None,
    }
}

/// What the keychain said about a network's session.
pub enum Stored {
    Found(String),
    /// There is no item: never signed in, or signed out.
    Absent,
    /// There is an item, or may be, and it could not be read: the user denied or cancelled the
    /// keychain prompt, or the keychain was locked. Not a sign-out: nothing may be deleted.
    Unreadable(String),
}

pub fn read(network: Network) -> Stored {
    if !enabled() {
        let name = format!("TINYCHAT_SESSION_{}", network.label().to_uppercase());
        return std::env::var(name).map_or(Stored::Absent, Stored::Found);
    }
    if !persistent() {
        return Stored::Unreadable(NO_STORE.to_owned());
    }
    let entry = match Entry::new(SERVICE, account(network)) {
        Ok(entry) => entry,
        Err(err) => return Stored::Unreadable(err.to_string()),
    };
    match entry.get_password() {
        Ok(session) => Stored::Found(session),
        Err(keyring::Error::NoEntry) => Stored::Absent,
        Err(err) => Stored::Unreadable(err.to_string()),
    }
}

pub fn save(network: Network, session: &str) -> Result<(), String> {
    if !enabled() {
        return Err("sessions are not kept while TINYCHAT_DIR is set".to_owned());
    }
    if !persistent() {
        return Err(NO_STORE.to_owned());
    }
    Entry::new(SERVICE, account(network))
        .map_err(|err| format!("keychain: {err}"))?
        .set_password(session)
        .map_err(|err| format!("keychain: {err}"))
}

/// A network reissued part of a session while it was in use (Slack rotates its `d` cookie).
/// `rewrite` is given the stored session and answers it with the new part in place, or `None`
/// to leave it alone. One at a time, so two accounts of one sign-in cannot undo each other.
/// True if the keychain now holds the rewritten session.
///
/// In a scratch directory nothing is kept in the keychain: the session rewritten is the one a
/// test handed over, and the result goes to `session.<network>` in that directory (readable by
/// the owner only) so the test can see what would have been saved.
pub fn rewrite(network: Network, rewrite: impl FnOnce(&str) -> Option<String>) -> Result<bool, String> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(stored) = load(network) else {
        return Ok(false);
    };
    let Some(fresh) = rewrite(&stored).filter(|fresh| *fresh != stored) else {
        return Ok(false);
    };
    if enabled() {
        return save(network, &fresh).map(|()| true);
    }
    use std::io::Write;
    let path = protocol::support_dir().join(format!("session.{}", network.label().to_lowercase()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut file = open.open(path).map_err(|err| err.to_string())?;
    file.write_all(fresh.as_bytes()).map_err(|err| err.to_string())?;
    Ok(false)
}

pub fn forget(network: Network) {
    if enabled() {
        let _ = Entry::new(SERVICE, account(network)).and_then(|entry| entry.delete_credential());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_build_keeps_sign_ins_past_a_restart() {
        // A build without a system store falls back to keyring's mock: it would find no
        // sessions and say nothing. `persistent` is what makes chatd refuse instead.
        assert!(super::persistent());
        assert!(super::NO_STORE.ends_with('.') && !super::NO_STORE.contains("keyring"));
    }
}
