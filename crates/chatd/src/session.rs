//! Signed-in sessions, kept in the login keychain and nowhere else.
//!
//! One item per network, holding what that network's sign-in produced. A sign-in that yields
//! several accounts (Slack workspaces, Tiny servers) is still one item; `adapters::accounts` splits it.

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
    // A Windows credential holds the packed session as a secret, so the blob is read and
    // unpacked; one stored as text before the packing comes back through the blob it always was.
    if !retry_timeouts {
        return secret_of(entry);
    }
    read_with_unlock_retry(|| secret_of(entry), UNLOCK_RETRY_WINDOW, UNLOCK_RETRY_PAUSE)
}

fn secret_of(entry: &Entry) -> keyring::Result<String> {
    #[cfg(target_os = "windows")]
    {
        entry.get_secret().and_then(|blob| unpack_secret(&blob))
    }
    #[cfg(not(target_os = "windows"))]
    {
        entry.get_password()
    }
}

/// The session packed the way a Windows credential needs it: its blob holds at most 2560 bytes,
/// and Telegram's keys in hex alone take more as text. Deflate keeps it in the Credential Manager.
#[cfg(target_os = "windows")]
const PACKED: u8 = b'z';

#[cfg(target_os = "windows")]
fn pack(session: &str) -> Vec<u8> {
    let mut packed = vec![PACKED];
    packed.extend(miniz_oxide::deflate::compress_to_vec(session.as_bytes(), 6));
    packed
}

#[cfg(target_os = "windows")]
fn unpack_secret(blob: &[u8]) -> keyring::Result<String> {
    if blob.first() == Some(&PACKED) {
        let inflated = miniz_oxide::inflate::decompress_to_vec(&blob[1..])
            .map_err(|err| keyring::Error::PlatformFailure(Box::new(std::io::Error::other(err.to_string()))))?;
        return Ok(String::from_utf8_lossy(&inflated).into_owned());
    }
    // A session kept as text before the packing: the blob is its UTF-16 characters.
    let words: Vec<u16> = blob.chunks(2).map(|pair| u16::from_le_bytes([pair[0], pair.get(1).copied().unwrap_or(0)])).collect();
    Ok(String::from_utf16_lossy(&words))
}

/// Stores the session, packed where a text would top out at the platform's limit.
fn keep(entry: &Entry, session: &str) -> keyring::Result<()> {
    #[cfg(target_os = "windows")]
    {
        entry.set_secret(&pack(session))
    }
    #[cfg(not(target_os = "windows"))]
    {
        entry.set_password(session)
    }
}

/// The keychain service before the product was renamed. A session found only there is moved to
/// `SERVICE` the first time it is read, so the rename signs nobody out.
const RENAMED_FROM: &str = "tinychat";

/// False when `TINYCLIENT_DIR` points chatd at a scratch directory. Tests and the demo never touch
/// the keychain and never sign in.
pub fn enabled() -> bool {
    std::env::var_os("TINYCLIENT_DIR").is_none()
}

/// The store sign-ins are kept in, named the way this system names it.
pub const STORE: &str = if cfg!(target_os = "macos") {
    "the keychain"
} else if cfg!(windows) {
    "Credential Manager"
} else {
    "the system keyring"
};

/// Said instead of reading or saving when this build's keyring keeps nothing past the process
/// (its in-memory mock, chosen when no system store was compiled in) or past a reboot.
pub const NO_STORE: &str = if cfg!(target_os = "macos") {
    "This copy of tinyclient has no keychain to keep sign-ins in, so it signs nobody in. Install a build for this system."
} else if cfg!(windows) {
    "This copy of tinyclient cannot keep sign-ins in Credential Manager, so it signs nobody in. Install a build for this system."
} else {
    "This copy of tinyclient has no system keyring to keep sign-ins in, so it signs nobody in. Install a build for this system."
};

/// An account's line while its session is read for over a second. macOS and the Secret Service
/// may be asking the person; Credential Manager never asks, so on Windows the read is only slow.
pub const STORE_WAITING: &str = if cfg!(target_os = "macos") {
    "Waiting for permission to read your sign-in from the keychain."
} else if cfg!(windows) {
    "Still reading your sign-in from Credential Manager."
} else {
    "Waiting for permission to read your sign-in from the system keyring."
};

/// An account's line when its session could not be read.
pub const STORE_REFUSED: &str = if cfg!(target_os = "macos") {
    "tinyclient can't read your sign-in from the keychain. You are still signed in."
} else if cfg!(windows) {
    "tinyclient can't read your sign-in from Credential Manager. You are still signed in."
} else {
    "tinyclient can't read your sign-in from the system keyring. You are still signed in."
};

/// Whether sign-ins are kept until deleted: the macOS keychain, the Windows credential store,
/// the Secret Service on Linux. Asked once.
pub fn persistent() -> bool {
    use keyring::credential::CredentialPersistence;
    static PERSISTENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PERSISTENT.get_or_init(|| {
        let persistence = keyring::default::default_credential_builder().persistence();
        let kept = matches!(persistence, CredentialPersistence::UntilDelete);
        if !kept {
            tracing::warn!("chatd: the keyring does not keep sign-ins past a restart; refusing to read or save any");
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
    // The unlock retry is the Linux Secret Service. macOS reads through `apple` instead.
    #[cfg(target_os = "macos")]
    let _ = retry_timeouts;
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
    #[cfg(target_os = "macos")]
    return apple::read(network);
    #[cfg(not(target_os = "macos"))]
    {
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
}

/// The session kept under the old name, now saved under the new one. The old item is deleted
/// only once the new one holds the session.
fn moved(network: Network, entry: &Entry, retry_timeouts: bool) -> Stored {
    let Ok(old) = Entry::new(RENAMED_FROM, account(network)) else {
        return Stored::Absent;
    };
    match read_password(&old, retry_timeouts) {
        Ok(session) => {
            if keep(entry, &session).is_ok() {
                let _ = old.delete_credential();
                #[cfg(target_os = "macos")]
                apple::trust_account(account(network));
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
    let saved = Entry::new(SERVICE, account(network))
        .map_err(|err| format!("keychain: {err}"))
        .and_then(|entry| keep(&entry, session).map_err(|err| format!("keychain: {err}")));
    // The item a sign-in just wrote trusts only this binary. Open it to later releases too.
    #[cfg(target_os = "macos")]
    if saved.is_ok() {
        apple::trust_account(account(network));
    }
    saved
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

/// macOS keeps one generic password per network in the login keychain. An ad-hoc release's
/// designated requirement is only its cdhash, so the next release is a different app and macOS
/// asks for the login password once per network. After this process has read or written an item,
/// its access list allows any app on this Mac, which is what makes the next release silent.
/// The keychain is still locked when the person is logged out. A Developer ID signature can
/// limit that list to chatd; this release is not signed with one.
#[cfg(target_os = "macos")]
mod apple {
    use super::{account, moved, Stored, SERVICE};
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    use protocol::Network;
    use security_framework::os::macos::access::SecAccess;
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework::os::macos::keychain_item::SecKeychainItem;
    use security_framework::os::macos::passwords::find_generic_password;
    use security_framework_sys::base::{errSecItemNotFound, SecAccessRef};
    use std::os::raw::c_void;
    use std::ptr;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecAccessCreate(descriptor: CFTypeRef, trusted: CFTypeRef, access: *mut SecAccessRef) -> i32;
        fn SecAccessCopyACLList(access: SecAccessRef, list: *mut CFTypeRef) -> i32;
        fn SecACLSetContents(acl: CFTypeRef, apps: CFTypeRef, description: CFTypeRef, prompt: u16) -> i32;
        fn SecACLCreateWithSimpleContents(
            access: SecAccessRef,
            apps: CFTypeRef,
            description: CFTypeRef,
            prompt: u16,
            new_acl: *mut CFTypeRef,
        ) -> i32;
        fn SecKeychainItemSetAccess(item: CFTypeRef, access: SecAccessRef) -> i32;
    }

    pub(super) fn read(network: Network) -> Stored {
        let name = account(network);
        let keychain = match SecKeychain::default() {
            Ok(keychain) => keychain,
            Err(err) => return Stored::Unreadable(err.to_string()),
        };
        match find_generic_password(Some(&[keychain]), SERVICE, name) {
            Ok((password, item)) => {
                let session = match String::from_utf8(password.as_ref().to_vec()) {
                    Ok(session) => session,
                    Err(_) => return Stored::Unreadable("the keychain item was not text".to_owned()),
                };
                // `password` frees its bytes on drop. The item stays, and this process may change it.
                drop(password);
                trust_item(name, &item);
                Stored::Found(session)
            }
            Err(err) if err.code() == errSecItemNotFound => match keyring::Entry::new(SERVICE, name) {
                Ok(entry) => moved(network, &entry, false),
                Err(err) => Stored::Unreadable(err.to_string()),
            },
            Err(err) => Stored::Unreadable(err.to_string()),
        }
    }

    pub(super) fn trust_account(name: &str) {
        let Ok(keychain) = SecKeychain::default() else {
            return;
        };
        match find_generic_password(Some(&[keychain]), SERVICE, name) {
            Ok((password, item)) => {
                drop(password);
                trust_item(name, &item);
            }
            Err(err) => tracing::warn!("chatd: {name}: could not open the saved sign-in to keep it ({err})"),
        }
    }

    fn trust_item(name: &str, item: &SecKeychainItem) {
        if let Err(err) = allow_anyone(item) {
            tracing::warn!("chatd: {name}: the sign-in will ask again after the next update ({err})");
        }
    }

    /// `nil` applications means any app may read the item without a prompt. An empty list would
    /// trust nobody. Apple documents the difference on `SecACLSetContents`.
    fn allow_anyone(item: &SecKeychainItem) -> Result<(), String> {
        let label = CFString::from("tinyclient");
        let label_ref = label.as_concrete_TypeRef();
        unsafe {
            let mut created: SecAccessRef = ptr::null_mut();
            let status = SecAccessCreate(label_ref.cast(), ptr::null(), &mut created);
            if status != 0 {
                return Err(format!("keychain access {status}"));
            }
            let access = SecAccess::wrap_under_create_rule(created);
            let mut list: CFTypeRef = ptr::null();
            let status = SecAccessCopyACLList(access.as_concrete_TypeRef(), &mut list);
            if status != 0 || list.is_null() {
                return Err(format!("keychain access list {status}"));
            }
            let acls = CFArray::<*const c_void>::wrap_under_create_rule(list.cast());
            if acls.is_empty() {
                let mut acl: CFTypeRef = ptr::null();
                let status = SecACLCreateWithSimpleContents(access.as_concrete_TypeRef(), ptr::null(), label_ref.cast(), 0, &mut acl);
                if status != 0 {
                    return Err(format!("keychain access entry {status}"));
                }
                if !acl.is_null() {
                    CFRelease(acl);
                }
            } else {
                for acl in acls.iter() {
                    let status = SecACLSetContents(*acl, ptr::null(), label_ref.cast(), 0);
                    if status != 0 {
                        return Err(format!("keychain access entry {status}"));
                    }
                }
            }
            let status = SecKeychainItemSetAccess(item.as_concrete_TypeRef().cast(), access.as_concrete_TypeRef());
            if status != 0 {
                return Err(format!("keychain access set {status}"));
            }
        }
        Ok(())
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

    /// Telegram's session holds every datacenter's key in hex alone: as a password that is
    /// longer than a Windows credential keeps, and it was silently refused. The packed blob
    /// must take it, and what an earlier release stored as text must still come back.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_session_too_long_as_text_still_keeps_its_words() {
        let key = "ab".repeat(256);
        let keys: Vec<String> = (1..=5)
            .map(|id| format!(r#"{{"id":{id},"v4":"149.154.167.{id}:443","v6":"[2001:67c:4e8:f004::a]:443","key":"{key}"}}"#))
            .collect();
        let session = format!(r#"{{"user":7001,"name":"Mira Kessler","home":2,"dcs":[{}]}}"#, keys.join(","));
        // As UTF-16 the text itself is already over what a credential keeps.
        assert!(session.len() > 1280, "the test session should not fit a Windows password");
        let packed = super::pack(&session);
        assert!(packed.len() <= 2560, "the packed session should fit a Windows credential");
        assert_eq!(super::unpack_secret(&packed).unwrap(), session);
        // What one stored as a text before the packing was its UTF-16 bytes.
        let legacy: Vec<u8> = session.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(super::unpack_secret(&legacy).unwrap(), session);
    }

    #[test]
    fn this_build_keeps_sign_ins_past_a_restart() {
        // A build without a system store falls back to keyring's mock: it would find no
        // sessions and say nothing. `persistent` is what makes chatd refuse instead.
        assert!(super::persistent());
        assert!(super::NO_STORE.ends_with('.') && super::NO_STORE.contains(super::STORE.trim_start_matches("the ")));
    }

    #[test]
    fn each_system_names_its_own_store() {
        use super::{NO_STORE, STORE, STORE_REFUSED, STORE_WAITING};
        assert!(STORE_REFUSED.contains(STORE) && STORE_REFUSED.ends_with("You are still signed in."));
        assert!(STORE_WAITING.contains(STORE));
        if cfg!(target_os = "macos") {
            assert_eq!(STORE_WAITING, "Waiting for permission to read your sign-in from the keychain.");
        } else {
            for words in [STORE, NO_STORE, STORE_WAITING, STORE_REFUSED] {
                assert!(!words.contains("keychain"), "{words}");
            }
        }
        if cfg!(windows) {
            // Credential Manager puts up no prompt to wait on.
            assert_eq!(STORE_WAITING, "Still reading your sign-in from Credential Manager.");
        }
    }
}
