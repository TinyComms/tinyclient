//! The computer this build runs on, named the way that computer names itself.
//!
//! The window used to say "this Mac" on every system. These words are what a person reads.

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
pub const FOLLOWS_LANGUAGE: &str = if cfg!(target_os = "macos") {
    "tinyclient follows the language of macOS."
} else if cfg!(windows) {
    "tinyclient follows the language of Windows."
} else {
    "tinyclient follows the language of the system."
};

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

/// Settings › Appearance, under text size. Reduce Motion is read from macOS only; `caption`
/// shows the keys as Ctrl elsewhere.
pub const APPEARANCE_CAPTION: &str = if cfg!(target_os = "macos") {
    "Text size is also ⌘+, ⌘− and ⌘0. Reduce Motion follows System Settings."
} else {
    "Text size is also ⌘+, ⌘− and ⌘0."
};

/// Settings › Shortcuts, above the list. Only macOS lets a person rebind an app's menu keys.
pub const SHORTCUTS_CAPTION: &str = if cfg!(target_os = "macos") {
    "To change a shortcut, add it in System Settings › Keyboard › Keyboard Shortcuts › App Shortcuts. The call keys are in no menu, so they cannot be changed there."
} else {
    "Shortcuts cannot be changed yet."
};

/// Settings › Notifications, N10: what draws attention to something new in the background.
pub const FLASH_LABEL: &str = if cfg!(target_os = "macos") { "Bounce the Dock icon" } else { "Flash the taskbar button" };

/// Settings › Notifications, N11: where the unread number is drawn.
pub const BADGE_LABEL: &str = if cfg!(target_os = "macos") { "Show the unread count on the Dock icon" } else { "Show the unread count on the taskbar button" };

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

        if cfg!(target_os = "macos") {
            assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the macOS keychain on this Mac.");
            assert_eq!(REVEAL, "Show in Finder");
            assert_eq!(STORE_WAITING, "Waiting for keychain access");
            assert!(STORE_WAITING_LINE.contains("Always Allow"));
            assert_eq!(FOLLOWS_LANGUAGE, "tinyclient follows the language of macOS.");
            assert!(APPEARANCE_CAPTION.ends_with("Reduce Motion follows System Settings."));
            assert!(SHORTCUTS_CAPTION.contains("App Shortcuts"));
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
                assert_eq!(STORE_WAITING, "Waiting for credential access");
                assert!(STORE_WAITING_LINE.contains("Credential Manager") && !STORE_WAITING_LINE.contains("asking"));
            } else {
                assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the system keyring on this computer.");
                assert_eq!(REVEAL, "Show in folder");
                assert_eq!(STORE_WAITING, "Waiting for keyring access");
                assert!(STORE_WAITING_LINE.contains("system keyring"));
                assert_eq!(FOLLOWS_LANGUAGE, "tinyclient follows the language of the system.");
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

    #[cfg(target_os = "linux")]
    #[test]
    fn os_release_does_not_call_this_machine_a_mac() {
        let name = os_release_name();
        assert!(!name.is_empty());
        assert!(!claims_to_be_a_mac(&name), "{name}");
    }
}
