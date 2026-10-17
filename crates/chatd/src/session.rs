//! Signed-in sessions, kept in the login keychain and nowhere else.
//!
//! One item per network, holding what that network's sign-in produced. A sign-in that yields
//! several accounts (Slack workspaces) is still one item; `adapters::accounts` splits it.

use keyring::Entry;
use protocol::Network;

const SERVICE: &str = "tinyclient";
// Some providers wait for GPG inside GetSecret, outside the Secret Service prompt
// protocol. The backend's two-second method timeout must not end the whole read.
const UNLOCK_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
const UNLOCK_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

fn unlock_timeout(error: &keyring::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut error: &(dyn std::error::Error + 'static) = error;
        loop {
            if let Some(error) = error.downcast_ref::<dbus::Error>() {
                return matches!(error.name(), Some("org.freedesktop.DBus.Error.NoReply"
                    | "org.freedesktop.DBus.Error.Timeout" | "org.freedesktop.DBus.Error.TimedOut"));
            }
            let Some(source) = error.source() else { return false };
            error = source;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = error;
        false
    }
}

fn read_with_unlock_retry(
    mut read: impl FnMut() -> keyring::Result<String>,
    window: std::time::Duration,
    pause: std::time::Duration,
) -> keyring::Result<String> {
    let began = std::time::Instant::now();
    loop {
        let result = read();
        let Err(error) = &result else { return result };
        if !unlock_timeout(error) || began.elapsed() >= window {
            return result;
        }
        std::thread::sleep(pause.min(window.saturating_sub(began.elapsed())));
        if began.elapsed() >= window {
            return result;
        }
    }
}

fn read_password(entry: &Entry, retry_timeouts: bool) -> keyring::Result<String> {
    if !retry_timeouts {
        return entry.get_password();
    }
    read_with_unlock_retry(|| entry.get_password(), UNLOCK_RETRY_WINDOW, UNLOCK_RETRY_PAUSE)
}

/// The keychain service before the product was renamed. A session found only there is moved to
/// `SERVICE` the first time it is read, so the rename signs nobody out.
const RENAMED_FROM: &str = "tinychat";

/// False when `TINYCLIENT_DIR` points chatd at a scratch directory. Tests and the demo never touch
/// the keychain and never sign in.
pub fn enabled() -> bool {
    std::env::var_os("TINYCLIENT_DIR").is_none()
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
/// session is one a test hands over in `TINYCLIENT_SESSION_DISCORD` or `TINYCLIENT_SESSION_SLACK`,
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
    read_session(network, false)
}

/// Background account loading can wait for GPG to unlock after a short D-Bus timeout.
/// Other reads keep their original behavior rather than extending a user action.
pub fn read_waiting(network: Network) -> Stored {
    read_session(network, true)
}

fn read_session(network: Network, retry_timeouts: bool) -> Stored {
    if !enabled() {
        let name = format!("TINYCLIENT_SESSION_{}", network.label().to_uppercase());
        // Scratch-only simulation of a provider whose GetSecret times out until unlocked.
        #[cfg(target_os = "linux")]
        if retry_timeouts && network == Network::Signal && std::env::var_os("TINYCLIENT_TEST_KEYCHAIN_UNLOCK").is_some() {
            let mut attempts = 0;
            let result = read_with_unlock_retry(|| {
                attempts += 1;
                let dir = protocol::support_dir();
                let _ = std::fs::write(dir.join("keychain-attempts"), attempts.to_string());
                if !dir.join("keychain-unlocked").exists() {
                    return Err(keyring::Error::PlatformFailure(Box::new(dbus::Error::new_custom(
                        "org.freedesktop.DBus.Error.NoReply", "test provider is still locked",
                    ))));
                }
                std::env::var(&name).map_err(|_| keyring::Error::NoEntry)
            }, UNLOCK_RETRY_WINDOW, UNLOCK_RETRY_PAUSE);
            return match result {
                Ok(session) => Stored::Found(session),
                Err(keyring::Error::NoEntry) => Stored::Absent,
                Err(err) => Stored::Unreadable(err.to_string()),
            };
        }
        return std::env::var(name).map_or(Stored::Absent, Stored::Found);
    }
    if !persistent() {
        return Stored::Unreadable(NO_STORE.to_owned());
    }
    let entry = match Entry::new(SERVICE, account(network)) {
        Ok(entry) => entry,
        Err(err) => return Stored::Unreadable(err.to_string()),
    };
    match read_password(&entry, retry_timeouts) {
        Ok(session) => Stored::Found(session),
        Err(keyring::Error::NoEntry) => moved(network, &entry, retry_timeouts),
        Err(err) => Stored::Unreadable(err.to_string()),
    }
}

/// The session kept under the old name, now saved under the new one. The old item is deleted
/// only once the new one holds the session.
fn moved(network: Network, entry: &Entry, retry_timeouts: bool) -> Stored {
    let Ok(old) = Entry::new(RENAMED_FROM, account(network)) else {
        return Stored::Absent;
    };
    match read_password(&old, retry_timeouts) {
        Ok(session) => {
            if entry.set_password(&session).is_ok() {
                let _ = old.delete_credential();
            }
            Stored::Found(session)
        }
        Err(keyring::Error::NoEntry) => Stored::Absent,
        Err(err) => Stored::Unreadable(err.to_string()),
    }
}

pub fn save(network: Network, session: &str) -> Result<(), String> {
    if !enabled() {
        return Err("sessions are not kept while TINYCLIENT_DIR is set".to_owned());
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
        for service in [SERVICE, RENAMED_FROM] {
            let _ = Entry::new(service, account(network)).and_then(|entry| entry.delete_credential());
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    fn platform_error(name: &str) -> keyring::Error {
        // Exercise a nested source chain, as keyring wraps the Secret Service error.
        keyring::Error::PlatformFailure(Box::new(keyring::Error::PlatformFailure(Box::new(
            dbus::Error::new_custom(name, "localized provider error"),
        ))))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dbus_timeouts_are_recognized_by_name_not_message() {
        for name in ["NoReply", "Timeout", "TimedOut"] {
            assert!(super::unlock_timeout(&platform_error(&format!("org.freedesktop.DBus.Error.{name}"))));
        }
        for name in ["AccessDenied", "Failed", "ServiceUnknown", "Disconnected"] {
            assert!(!super::unlock_timeout(&platform_error(&format!("org.freedesktop.DBus.Error.{name}"))));
        }
        assert!(!super::unlock_timeout(&keyring::Error::NoEntry));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timed_out_read_recovers_after_unlocking() {
        let mut attempts = 0;
        let result = super::read_with_unlock_retry(|| {
            attempts += 1;
            if attempts < 3 { Err(platform_error("org.freedesktop.DBus.Error.NoReply")) }
            else { Ok("saved session".to_owned()) }
        }, std::time::Duration::from_secs(1), std::time::Duration::ZERO);
        assert_eq!(result.unwrap(), "saved session");
        assert_eq!(attempts, 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refusals_and_expired_retry_windows_stop_reading() {
        for name in ["AccessDenied", "Failed", "NoReply"] {
            let mut attempts = 0;
            let window = if name == "NoReply" { std::time::Duration::ZERO } else { std::time::Duration::from_secs(1) };
            let result = super::read_with_unlock_retry(|| {
                attempts += 1;
                Err(platform_error(&format!("org.freedesktop.DBus.Error.{name}")))
            }, window, std::time::Duration::ZERO);
            assert!(result.is_err());
            assert_eq!(attempts, 1);
        }
    }
    #[test]
    fn missing_credentials_and_dismissed_prompts_are_not_retried() {
        for error in [
            keyring::Error::NoEntry,
            keyring::Error::NoStorageAccess(Box::new(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied, "prompt dismissed",
            ))),
        ] {
            let mut error = Some(error);
            let result = super::read_with_unlock_retry(
                || Err(error.take().expect("this read must not be retried")),
                std::time::Duration::from_secs(1), std::time::Duration::ZERO,
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn this_build_keeps_sign_ins_past_a_restart() {
        // A build without a system store falls back to keyring's mock: it would find no
        // sessions and say nothing. `persistent` is what makes chatd refuse instead.
        assert!(super::persistent());
        assert!(super::NO_STORE.ends_with('.') && !super::NO_STORE.contains("keyring"));
    }
}
