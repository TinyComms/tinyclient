//! The computer this build runs on, named the way that computer names itself.
//!
//! The window used to say "this Mac" on every system. These words are what a person reads.

use std::path::{Path, PathBuf};

/// "this Mac", "this computer", or "this PC".
pub const HERE: &str = if cfg!(target_os = "macos") {
    "this Mac"
} else if cfg!(windows) {
    "this PC"
} else {
    "this computer"
};

// The window is drawn as a phone (`gui::phone::set_active`): what it says of itself names the
// phone, never the computer a build happens to run on.
static PHONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set with the window's form, every frame.
pub fn set_phone(on: bool) {
    PHONE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Drawn as a phone, or running on one: a build for Android or iOS never names a computer, even
/// in a frame drawn before the form is known (a settings page, a tablet's desktop form).
fn phone() -> bool {
    PHONE.load(std::sync::atomic::Ordering::Relaxed) || cfg!(any(target_os = "android", target_os = "ios"))
}

/// [`HERE`], or "this phone" while the window is a phone.
pub fn here() -> &'static str {
    if phone() {
        "this phone"
    } else {
        HERE
    }
}

/// The phone's own system, for the words that name it.
const PHONE_SYSTEM: &str = if cfg!(target_os = "ios") {
    "iOS"
} else if cfg!(target_os = "android") {
    "Android"
} else {
    "system"
};

/// Settings › Privacy, the retention row, for the form the window is in.
pub fn keep_messages() -> &'static str {
    if phone() {
        "Keep messages on this phone for"
    } else {
        KEEP_MESSAGES
    }
}

/// Settings › About, under Copy diagnostics, for the form the window is in.
pub fn diagnostics() -> String {
    if phone() {
        format!("Version, {PHONE_SYSTEM}, memory and connection states. No messages, names or tokens.")
    } else {
        DIAGNOSTICS.to_owned()
    }
}

/// Welcome footnote: where a sign-in is kept.
pub const SIGN_IN_KEPT: &str = if cfg!(target_os = "macos") {
    "Your sign-in is kept in the macOS keychain on this Mac."
} else if cfg!(windows) {
    "Your sign-in is kept in Windows Credential Manager on this PC."
} else {
    "Your sign-in is kept in the system keyring on this computer."
};

/// [`SIGN_IN_KEPT`] for the form the window is in: a phone keeps it in its own store.
pub fn sign_in_kept() -> &'static str {
    if !phone() {
        SIGN_IN_KEPT
    } else if cfg!(target_os = "ios") {
        "Your sign-in is kept in the iOS keychain on this phone."
    } else {
        "Your sign-in is kept in tinyclient's private storage on this phone."
    }
}

/// The button that reveals a file in the system file manager.
pub const REVEAL: &str = if cfg!(target_os = "macos") {
    "Show in Finder"
} else if cfg!(windows) {
    "Show in File Explorer"
} else {
    "Show in folder"
};

/// The viewer's glance at a picture. Only macOS has Quick Look; the others open it.
pub const QUICK_LOOK: &str = if cfg!(target_os = "macos") { "Quick Look" } else { "Preview" };

/// The system's app for a picture. Preview is the macOS app; elsewhere it is the default.
pub const OPEN_PICTURE: &str = if cfg!(target_os = "macos") { "Open in Preview" } else { "Open" };

/// What a successful save into Downloads says, before it offers [`REVEAL`].
pub const SAVED_DOWNLOADS: &str = "Saved to Downloads";

/// What a failed save into Downloads says.
pub const COULD_NOT_SAVE: &str = "Couldn't save to Downloads";

/// The save note, offering to show the file that was just written.
pub fn offer_reveal(note: &str) -> String {
    format!("{note} · {REVEAL}")
}

/// Sidebar banner while the system store asks permission to read saved sign-ins.
pub const STORE_WAITING: &str = if cfg!(target_os = "macos") {
    "Waiting for keychain access"
} else if cfg!(windows) {
    "Waiting for credential access"
} else {
    "Waiting for keyring access"
};

/// Empty-state line under [`STORE_WAITING`]. Credential Manager never asks, so on Windows the
/// read is only slow.
pub const STORE_WAITING_LINE: &str = if cfg!(target_os = "macos") {
    "macOS is asking to let tinyclient use its saved sign-ins. Choose Always Allow. The prompt may be behind this window."
} else if cfg!(windows) {
    "tinyclient is still reading its saved sign-ins from Windows Credential Manager."
} else {
    "The system keyring is asking to let tinyclient use its saved sign-ins. The prompt may be behind this window."
};

/// Settings search, when someone looks for a language setting.
pub const FOLLOWS_LANGUAGE: &str = "tinyclient is in English only for now. Dates and times follow your system.";

/// Settings search, when someone looks for sync.
pub const SETTINGS_STAY: &str = if cfg!(target_os = "macos") {
    "Settings stay on this Mac. Nothing is synced."
} else if cfg!(windows) {
    "Settings stay on this PC. Nothing is synced."
} else {
    "Settings stay on this computer. Nothing is synced."
};

/// Settings › Privacy, the retention row.
pub const KEEP_MESSAGES: &str = if cfg!(target_os = "macos") {
    "Keep messages on this Mac for"
} else if cfg!(windows) {
    "Keep messages on this PC for"
} else {
    "Keep messages on this computer for"
};

/// Settings › About, under Copy diagnostics.
pub const DIAGNOSTICS: &str = if cfg!(target_os = "macos") {
    "Version, macOS, memory and connection states. No messages, names, tokens, or log lines."
} else if cfg!(windows) {
    "Version, Windows, memory and connection states. No messages, names, tokens, or log lines."
} else {
    "Version, Linux, memory and connection states. No messages, names, tokens, or log lines."
};

/// Settings › Appearance, under text size. `caption` shows the keys as Ctrl off macOS.
pub const APPEARANCE_CAPTION: &str = if cfg!(target_os = "macos") {
    "Text size is also ⌘+, ⌘− and ⌘0. Reduce Motion follows System Settings."
} else if cfg!(any(windows, target_os = "linux")) {
    "Text size is also ⌘+, ⌘− and ⌘0. Reduce Motion follows the system animation setting."
} else {
    "Text size is also ⌘+, ⌘− and ⌘0."
};

/// Settings › Shortcuts, above the list. Click a row, then press the new keys.
pub const SHORTCUTS_CAPTION: &str = "Click a row, then press the new keys. Reset restores the defaults. A key another command uses is refused. Backspace on a row restores that one.";

/// Settings › Notifications, N10: what draws attention to something new in the background.
pub const FLASH_LABEL: &str = if cfg!(target_os = "macos") {
    "Bounce the Dock icon"
} else if cfg!(target_os = "linux") {
    "Ask the desktop for attention"
} else {
    "Flash the taskbar button"
};

/// Settings › Notifications, N11: where the unread number is drawn.
pub const BADGE_LABEL: &str = if cfg!(target_os = "macos") {
    "Show the unread count on the Dock icon"
} else if cfg!(target_os = "linux") {
    "Show the unread count on the launcher icon"
} else {
    "Show the unread count on the taskbar button"
};

/// Settings › Notifications, N13, after the test notification was posted.
pub const TEST_SENT: &str = if cfg!(target_os = "macos") {
    "Sent. If nothing appeared, check Focus and System Settings › Notifications."
} else if cfg!(windows) {
    "Sent. If nothing appeared, check Do not disturb in Windows Settings › System › Notifications."
} else {
    "Sent. If nothing appeared, check your desktop's Do Not Disturb."
};

/// Settings › Notifications, under Quiet time: where a system-wide schedule lives.
pub const FOCUS_CAPTION: &str = if cfg!(target_os = "macos") {
    "For schedules by place or app, use Focus in System Settings. tinyclient follows it."
} else if cfg!(windows) {
    "Do not disturb in Windows Settings › System › Notifications holds tinyclient's notifications too."
} else {
    "Your desktop's Do Not Disturb holds tinyclient's notifications too."
};

/// Settings search, when someone looks for a menu bar icon.
pub const NO_TRAY_ICON: &str = if cfg!(target_os = "macos") {
    "tinyclient has no menu bar icon yet."
} else if cfg!(windows) {
    "tinyclient has no system tray icon yet."
} else {
    "tinyclient has no tray icon yet."
};

/// `open` failed before a handler was asked.
pub const COULD_NOT_OPEN: &str = if cfg!(target_os = "macos") {
    "This Mac could not open the link."
} else if cfg!(windows) {
    "This PC could not open the link."
} else {
    "This computer could not open the link."
};

/// The user's Downloads folder: the system's own place, then `Downloads` under the home folder.
/// `HOME` is not required. Windows does not set it for a normal desktop session.
pub fn downloads_dir() -> Option<PathBuf> {
    system_downloads().filter(|path| path.is_dir()).or_else(guessed_downloads)
}

/// Where a save dialog opens: Downloads, else the home folder.
pub fn downloads_or_home() -> PathBuf {
    downloads_dir().or_else(home_dir).unwrap_or_default()
}

/// A free path inside `folder` for `name`. Only the last part of `name` is used, so a file is
/// never written outside `folder`. `photo.png` then `photo 2.png`, up to 99.
pub fn unused_in(folder: &Path, name: &str) -> Option<PathBuf> {
    let name = file_leaf(name);
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (1..100)
        .map(|n| if n == 1 { folder.join(name) } else { folder.join(format!("{stem} {n}{extension}")) })
        .find(|path| !path.exists())
}

/// Copies `from` onto `to` and marks the copy as downloaded. The mark stays: a failure to mark
/// does not undo the copy.
pub fn copy_downloaded(from: &Path, to: &Path) -> std::io::Result<PathBuf> {
    if let Some(parent) = to.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    if let Err(err) = protocol::quarantine::mark(to) {
        tracing::warn!("tinyclient: a saved file was not marked as downloaded: {err}");
    }
    Ok(to.to_path_buf())
}

/// `save_into_downloads`, once the folder is already known. The copy itself does no folder lookup.
pub fn save_into(folder: &Path, name: &str, from: &Path) -> Option<PathBuf> {
    copy_downloaded(from, &unused_in(folder, name)?).ok()
}

/// `XDG_DOWNLOAD_DIR` from the text of `user-dirs.dirs`. `$HOME` expands to `home` when the value
/// uses it. An absolute path does not need `home`.
pub fn xdg_download_from(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(value) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else { continue };
        found = expand_xdg(value.trim(), home);
    }
    found
}

/// The lab's save-downloads driver. A build without it opens the window and stays up.
pub fn prove_save_downloads() -> Result<(), &'static str> {
    if cfg!(target_os = "macos") {
        if QUICK_LOOK != "Quick Look" || OPEN_PICTURE != "Open in Preview" {
            return Err("macOS picture buttons were renamed");
        }
    } else if QUICK_LOOK == "Quick Look" || OPEN_PICTURE == "Open in Preview" {
        return Err("picture buttons still use the macOS names");
    }
    let offer = offer_reveal(SAVED_DOWNLOADS);
    if !offer.starts_with(SAVED_DOWNLOADS) || !offer.contains(REVEAL) {
        return Err("a save does not offer to show the file");
    }
    let parsed = xdg_download_from("XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n", Some(Path::new("/home/person")));
    if parsed.as_deref() != Some(Path::new("/home/person/Downloads")) {
        return Err("user-dirs download dir was not read");
    }
    if xdg_download_from("XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n", None).is_some() {
        return Err("user-dirs expanded $HOME with no home folder");
    }
    if cfg!(any(windows, target_os = "macos")) && without_home(downloads_dir).is_none() {
        return Err("Downloads was not found with HOME unset");
    }
    Ok(())
}

fn file_leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().filter(|name| !name.is_empty() && *name != "." && *name != "..").unwrap_or("file")
}

fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let home = PathBuf::from(home);
    (!home.as_os_str().is_empty()).then_some(home)
}

fn guessed_downloads() -> Option<PathBuf> {
    let home = home_dir()?;
    let downloads = home.join("Downloads");
    downloads.is_dir().then_some(downloads)
}

fn expand_xdg(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    let value = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(value);
    if let Some(rest) = value.strip_prefix("$HOME") {
        let home = home?;
        let rest = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\')).unwrap_or(rest);
        return Some(if rest.is_empty() { home.to_path_buf() } else { home.join(rest) });
    }
    (!value.is_empty()).then(|| PathBuf::from(value))
}

#[cfg(all(windows, feature = "gui"))]
fn system_downloads() -> Option<PathBuf> {
    windows_downloads()
}

#[cfg(all(target_os = "macos", feature = "gui"))]
fn system_downloads() -> Option<PathBuf> {
    macos_downloads()
}

#[cfg(all(target_os = "linux", feature = "gui"))]
fn system_downloads() -> Option<PathBuf> {
    linux_downloads()
}

#[cfg(not(all(feature = "gui", any(windows, target_os = "macos", target_os = "linux"))))]
fn system_downloads() -> Option<PathBuf> {
    None
}

/// `SHGetKnownFolderPath(FOLDERID_Downloads)`. Does not read `HOME`.
#[cfg(all(windows, feature = "gui"))]
fn windows_downloads() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
    unsafe {
        // A successful init, including one that only raised the count, is balanced below.
        let started = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut raw = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut raw);
        let path = if hr < 0 || raw.is_null() {
            if !raw.is_null() {
                CoTaskMemFree(raw.cast());
            }
            None
        } else {
            let mut len = 0usize;
            while len < 32_768 && *raw.add(len) != 0 {
                len += 1;
            }
            let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(raw, len)));
            CoTaskMemFree(raw.cast());
            (!path.as_os_str().is_empty()).then_some(path)
        };
        if started >= 0 {
            CoUninitialize();
        }
        path
    }
}

/// `NSSearchPathForDirectoriesInDomains(NSDownloadsDirectory)`. Does not read `HOME`.
#[cfg(all(target_os = "macos", feature = "gui"))]
fn macos_downloads() -> Option<PathBuf> {
    use std::ffi::{c_char, c_void, CStr};
    type Id = *mut c_void;
    #[link(name = "Foundation", kind = "framework")]
    extern "C" {
        fn NSSearchPathForDirectoriesInDomains(directory: usize, domain_mask: usize, expand_tilde: u8) -> Id;
    }
    #[link(name = "objc")]
    extern "C" {
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }
    // NSDownloadsDirectory = 15, NSUserDomainMask = 1.
    unsafe {
        let paths = NSSearchPathForDirectoriesInDomains(15, 1, 1);
        if paths.is_null() {
            return None;
        }
        let send0: extern "C" fn(Id, Id) -> usize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let at: extern "C" fn(Id, Id, usize) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let utf8: extern "C" fn(Id, Id) -> *const c_char = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let count = sel_registerName(c"count".as_ptr());
        if send0(paths, count) == 0 {
            return None;
        }
        let first = at(paths, sel_registerName(c"objectAtIndex:".as_ptr()), 0);
        if first.is_null() {
            return None;
        }
        let bytes = utf8(first, sel_registerName(c"UTF8String".as_ptr()));
        if bytes.is_null() {
            return None;
        }
        let path = PathBuf::from(CStr::from_ptr(bytes).to_string_lossy().as_ref());
        (!path.as_os_str().is_empty()).then_some(path)
    }
}

/// `XDG_DOWNLOAD_DIR` in `user-dirs.dirs`, under `$XDG_CONFIG_HOME` or `~/.config`.
#[cfg(all(target_os = "linux", feature = "gui"))]
fn linux_downloads() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home_dir().map(|home| home.join(".config")))?;
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    let home = home_dir();
    xdg_download_from(&text, home.as_deref())
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Runs `body` with `HOME` unset, then puts the previous value back.
fn without_home<T>(body: impl FnOnce() -> T) -> T {
    let _lock = env_lock();
    let saved = std::env::var_os("HOME");
    std::env::remove_var("HOME");
    struct PutBack(Option<std::ffi::OsString>);
    impl Drop for PutBack {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }
    let _put = PutBack(saved);
    body()
}

/// "{network} is offline · showing what's on this computer".
pub fn offline(network: &str) -> String {
    format!("{network} is offline · showing what's on {}", here())
}

/// "496 MB on this computer".
pub fn on_here(amount: &str) -> String {
    format!("{amount} on {}", here())
}

/// Nothing registered for a URL scheme.
pub fn nothing_opens(url: &str) -> String {
    format!("Nothing on {} opens {url}", here())
}

/// Accounts caption under the sign-out rows.
pub fn sign_out_caption() -> String {
    format!("Signing out removes this account's sign-in and everything tinyclient saved for it from {}.", here())
}

/// Sign-out confirmation, first paragraph.
pub fn sign_out_body(network: &str) -> String {
    format!("This removes from {} the saved sign-in and the saved messages, images and files of this account, and ends this session on {network}.", here())
}

/// Clear-cache confirmation.
pub fn clear_saved() -> String {
    format!("Everything tinyclient saved to show you quickly is deleted from {}. It comes back from the networks as you open conversations. Sign-ins, drafts and settings stay.", here())
}

/// P4, when nothing is old enough to delete yet.
pub fn retention_none_yet() -> String {
    format!("Nothing here is that old yet. Older messages will be removed from {} as they age.", here())
}

/// P4, the shared tail: "are removed from this computer. They stay on …".
pub fn removed_from_here() -> String {
    format!("are removed from {}. They stay on Slack, Discord and the others.", here())
}

/// `PRETTY_NAME` from os-release(5): `/etc/os-release`, else `/usr/lib/os-release`.
#[cfg(target_os = "linux")]
pub fn os_release_name() -> String {
    let text = ["/etc/os-release", "/usr/lib/os-release"].iter().find_map(|path| std::fs::read_to_string(path).ok());
    pretty_name(text.as_deref().unwrap_or_default())
}

/// The last `PRETTY_NAME` in os-release text, unquoted the way a shell would. "Linux" when it is
/// missing or empty, as os-release(5) says.
#[cfg(any(target_os = "linux", test))]
fn pretty_name(text: &str) -> String {
    text.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("PRETTY_NAME="))
        .map(unquote)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Linux".to_owned())
}

/// A value in single quotes is taken as written. Otherwise its double quotes come off and `\"`,
/// `\\`, `\$` and `` \` `` stand for the character.
#[cfg(any(target_os = "linux", test))]
fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('\'').and_then(|value| value.strip_suffix('\'')) {
        return inner.to_owned();
    }
    let inner = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        let escaped = if c == '\\' { chars.next_if(|next| matches!(next, '"' | '\\' | '$' | '`')) } else { None };
        out.push(escaped.unwrap_or(c));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_to_be_a_mac(text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.contains("mac") || lower.contains("finder") || lower.contains("keychain") || lower.contains("always allow")
    }

    #[test]
    fn the_welcome_line_names_this_computer_and_its_own_store() {
        assert!(SIGN_IN_KEPT.contains(HERE));
        assert!(SIGN_IN_KEPT.starts_with("Your sign-in is kept in "));
        assert!(SIGN_IN_KEPT.ends_with('.'));
        assert_eq!(KEEP_MESSAGES, format!("Keep messages on {HERE} for"));
        assert!(SETTINGS_STAY.contains(HERE));
        assert!(DIAGNOSTICS.contains(if cfg!(target_os = "macos") { "macOS" } else if cfg!(windows) { "Windows" } else { "Linux" }));
        assert!(COULD_NOT_OPEN.contains(if cfg!(target_os = "linux") { "computer" } else { HERE.trim_start_matches("this ") }));
        assert_eq!(offline("Discord"), format!("Discord is offline · showing what's on {HERE}"));
        assert_eq!(nothing_opens("discord://channels"), format!("Nothing on {HERE} opens discord://channels"));
        assert!(sign_out_body("Slack").contains("Slack") && sign_out_body("Slack").contains(HERE));
        assert!(clear_saved().contains(HERE));
        assert!(retention_none_yet().contains(HERE));
        assert!(removed_from_here().starts_with("are removed from "));
        assert_eq!(STORE_WAITING_LINE.ends_with("The prompt may be behind this window."), !cfg!(windows));
        assert!(APPEARANCE_CAPTION.starts_with("Text size is also ⌘+, ⌘− and ⌘0."));

        assert_eq!(FOLLOWS_LANGUAGE, "tinyclient is in English only for now. Dates and times follow your system.");
        if cfg!(target_os = "macos") {
            assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the macOS keychain on this Mac.");
            assert_eq!(REVEAL, "Show in Finder");
            assert_eq!(QUICK_LOOK, "Quick Look");
            assert_eq!(OPEN_PICTURE, "Open in Preview");
            assert_eq!(STORE_WAITING, "Waiting for keychain access");
            assert!(STORE_WAITING_LINE.contains("Always Allow"));
            assert!(APPEARANCE_CAPTION.ends_with("Reduce Motion follows System Settings."));
            assert!(SHORTCUTS_CAPTION.contains("Click a row"));
            assert!(SHORTCUTS_CAPTION.contains("Reset"));
            assert_eq!(FLASH_LABEL, "Bounce the Dock icon");
            assert!(FOCUS_CAPTION.contains("Focus"));
            assert_eq!(NO_TRAY_ICON, "tinyclient has no menu bar icon yet.");
        } else {
            let shown = [
                SIGN_IN_KEPT,
                REVEAL,
                STORE_WAITING,
                STORE_WAITING_LINE,
                FOLLOWS_LANGUAGE,
                SETTINGS_STAY,
                KEEP_MESSAGES,
                DIAGNOSTICS,
                APPEARANCE_CAPTION,
                SHORTCUTS_CAPTION,
                FLASH_LABEL,
                BADGE_LABEL,
                FOCUS_CAPTION,
                TEST_SENT,
                NO_TRAY_ICON,
                COULD_NOT_OPEN,
                QUICK_LOOK,
                OPEN_PICTURE,
                HERE,
            ];
            for words in shown {
                assert!(!claims_to_be_a_mac(words), "{words}");
            }
            for words in [offline("Discord"), sign_out_caption(), sign_out_body("Slack"), clear_saved(), retention_none_yet(), removed_from_here(), on_here("12 MB")] {
                assert!(!claims_to_be_a_mac(&words), "{words}");
            }
            if cfg!(windows) {
                assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in Windows Credential Manager on this PC.");
                assert_eq!(REVEAL, "Show in File Explorer");
                assert_eq!(QUICK_LOOK, "Preview");
                assert_eq!(OPEN_PICTURE, "Open");
                assert_eq!(STORE_WAITING, "Waiting for credential access");
                assert!(STORE_WAITING_LINE.contains("Credential Manager") && !STORE_WAITING_LINE.contains("asking"));
            } else {
                assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the system keyring on this computer.");
                assert_eq!(REVEAL, "Show in folder");
                assert_eq!(QUICK_LOOK, "Preview");
                assert_eq!(OPEN_PICTURE, "Open");
                assert_eq!(STORE_WAITING, "Waiting for keyring access");
                assert!(STORE_WAITING_LINE.contains("system keyring"));
            }
        }
    }

    #[test]
    fn os_release_names_the_system_as_a_shell_would_read_it() {
        let name = |line: &str| pretty_name(&format!("NAME=Fedora\nID=fedora\n{line}\nVERSION_ID=40\n"));
        assert_eq!(name(r#"PRETTY_NAME="Fedora Linux 40 (Workstation Edition)""#), "Fedora Linux 40 (Workstation Edition)");
        assert_eq!(name("PRETTY_NAME='Arch Linux'"), "Arch Linux");
        assert_eq!(name(r#"PRETTY_NAME='Costs $5, "quoted" \`as is\`'"#), r#"Costs $5, "quoted" \`as is\`"#);
        assert_eq!(name(r#"PRETTY_NAME="Say \"hi\" for \$5 \`now\` \\ \n""#), r#"Say "hi" for $5 `now` \ \n"#);
        assert_eq!(name(r#"PRETTY_NAME="a\\$b""#), r#"a\$b"#);
        assert_eq!(name("PRETTY_NAME=Alpine"), "Alpine");
        assert_eq!(name("# PRETTY_NAME=\"Commented\""), "Linux");
        assert_eq!(name(r#"PRETTY_NAME="""#), "Linux");
        assert_eq!(pretty_name(""), "Linux");
        assert_eq!(pretty_name("PRETTY_NAME=\"First\"\nPRETTY_NAME=\"Second\"\n"), "Second");
    }

    #[test]
    fn downloads_is_found_with_home_unset() {
        // Linux reads user-dirs.dirs. Unsetting HOME can leave that file unread, so the live
        // folder is required only where the system API does not use HOME.
        let found = without_home(downloads_dir);
        if cfg!(any(windows, target_os = "macos")) {
            assert!(found.is_some(), "Downloads needs no HOME");
        }
        assert_eq!(prove_save_downloads(), Ok(()));
    }

    #[test]
    fn user_dirs_names_downloads_without_reading_home() {
        let text = "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\n";
        assert_eq!(xdg_download_from(text, Some(Path::new("/home/person"))).unwrap(), Path::new("/home/person/Téléchargements"));
        assert_eq!(xdg_download_from("XDG_DOWNLOAD_DIR=\"/data/downloads\"\n", None).unwrap(), Path::new("/data/downloads"));
        assert!(xdg_download_from("# XDG_DOWNLOAD_DIR=\"/nope\"\nXDG_DOWNLOAD_DIR=\n", None).is_none());
        assert_eq!(
            xdg_download_from("XDG_DOWNLOAD_DIR=\"/first\"\nXDG_DOWNLOAD_DIR=\"/second\"\n", None).unwrap(),
            Path::new("/second"),
        );
    }

    #[test]
    fn a_save_stays_inside_the_folder_and_is_marked_downloaded() {
        let dir = std::env::temp_dir().join(format!("tinyclient-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = unused_in(&dir, "../outside.txt").unwrap();
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert_eq!(path.file_name().unwrap(), "outside.txt");
        assert_eq!(unused_in(&dir, "..").unwrap().file_name().unwrap(), "file");
        std::fs::write(dir.join("photo.png"), b"a").unwrap();
        assert_eq!(unused_in(&dir, "photo.png").unwrap().file_name().unwrap(), "photo 2.png");
        std::fs::write(dir.join("from.bin"), b"hello").unwrap();
        let to = copy_downloaded(&dir.join("from.bin"), &dir.join("to.bin")).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"hello");
        #[cfg(windows)]
        {
            let mut stream = to.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            let zone = std::fs::read(stream).unwrap();
            assert!(std::str::from_utf8(&zone).unwrap().contains("ZoneId=3"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(all(windows, feature = "gui"))]
    #[test]
    fn the_known_downloads_folder_does_not_read_home() {
        let known = without_home(windows_downloads);
        assert!(known.is_some(), "the system Downloads folder was not found");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn os_release_does_not_call_this_machine_a_mac() {
        let name = os_release_name();
        assert!(!name.is_empty());
        assert!(!claims_to_be_a_mac(&name), "{name}");
    }
}
