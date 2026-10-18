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

/// Welcome footnote: where a sign-in is kept.
pub const SIGN_IN_KEPT: &str = if cfg!(target_os = "macos") {
    "Your sign-in is kept in the macOS keychain on this Mac."
} else if cfg!(windows) {
    "Your sign-in is kept in Windows Credential Manager on this PC."
} else {
    "Your sign-in is kept in the system keyring on this computer."
};

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

/// Empty-state line under [`STORE_WAITING`].
pub const STORE_WAITING_LINE: &str = if cfg!(target_os = "macos") {
    "macOS is asking to let tinyclient use its saved sign-ins. Choose Always Allow. The prompt may be behind this window."
} else if cfg!(windows) {
    "Windows is asking to let tinyclient use its saved sign-ins. The prompt may be behind this window."
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
    "Version, macOS, memory and connection states. No messages, names or tokens."
} else if cfg!(windows) {
    "Version, Windows, memory and connection states. No messages, names or tokens."
} else {
    "Version, Linux, memory and connection states. No messages, names or tokens."
};

/// `open` failed before a handler was asked.
pub const COULD_NOT_OPEN: &str = if cfg!(target_os = "macos") {
    "This Mac could not open the link."
} else if cfg!(windows) {
    "This PC could not open the link."
} else {
    "This computer could not open the link."
};

/// The operating system a diagnostics line should name.
pub const OS_NAME: &str = if cfg!(target_os = "macos") {
    "macOS"
} else if cfg!(windows) {
    "Windows"
} else {
    "Linux"
};

/// "{network} is offline · showing what's on this computer".
pub fn offline(network: &str) -> String {
    format!("{network} is offline · showing what's on {HERE}")
}

/// "496 MB on this computer".
pub fn on_here(amount: &str) -> String {
    format!("{amount} on {HERE}")
}

/// Nothing registered for a URL scheme.
pub fn nothing_opens(url: &str) -> String {
    format!("Nothing on {HERE} opens {url}")
}

/// Accounts caption under the sign-out rows.
pub fn sign_out_caption() -> String {
    format!("Signing out removes this account's sign-in and everything tinyclient saved for it from {HERE}.")
}

/// Sign-out confirmation, first paragraph.
pub fn sign_out_body(network: &str) -> String {
    format!("This removes from {HERE} the saved sign-in and the saved messages, images and files of this account, and ends this session on {network}.")
}

/// Clear-cache confirmation.
pub fn clear_saved() -> String {
    format!("Everything tinyclient saved to show you quickly is deleted from {HERE}. It comes back from the networks as you open conversations. Sign-ins, drafts and settings stay.")
}

/// P4, when nothing is old enough to delete yet.
pub fn retention_none_yet() -> String {
    format!("Nothing here is that old yet. Older messages will be removed from {HERE} as they age.")
}

/// P4, the shared tail: "are removed from this computer. They stay on …".
pub fn removed_from_here() -> String {
    format!("are removed from {HERE}. They stay on Slack, Discord and the others.")
}

/// `PRETTY_NAME` from `/etc/os-release`.
#[cfg(target_os = "linux")]
pub fn os_release_name() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        let Some(value) = line.strip_prefix("PRETTY_NAME=") else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
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
        assert!(DIAGNOSTICS.contains(OS_NAME));
        assert!(COULD_NOT_OPEN.contains(if cfg!(target_os = "linux") { "computer" } else { HERE.trim_start_matches("this ") }));
        assert_eq!(offline("Discord"), format!("Discord is offline · showing what's on {HERE}"));
        assert_eq!(nothing_opens("discord://channels"), format!("Nothing on {HERE} opens discord://channels"));
        assert!(sign_out_body("Slack").contains("Slack") && sign_out_body("Slack").contains(HERE));
        assert!(clear_saved().contains(HERE));
        assert!(retention_none_yet().contains(HERE));
        assert!(removed_from_here().starts_with("are removed from "));
        assert!(STORE_WAITING_LINE.ends_with("The prompt may be behind this window."));

        if cfg!(target_os = "macos") {
            assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the macOS keychain on this Mac.");
            assert_eq!(REVEAL, "Show in Finder");
            assert_eq!(STORE_WAITING, "Waiting for keychain access");
            assert!(STORE_WAITING_LINE.contains("Always Allow"));
            assert_eq!(FOLLOWS_LANGUAGE, "tinyclient follows the language of macOS.");
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
            } else {
                assert_eq!(SIGN_IN_KEPT, "Your sign-in is kept in the system keyring on this computer.");
                assert_eq!(REVEAL, "Show in folder");
                assert_eq!(STORE_WAITING, "Waiting for keyring access");
                assert!(STORE_WAITING_LINE.contains("system keyring"));
                assert_eq!(FOLLOWS_LANGUAGE, "tinyclient follows the language of the system.");
                assert_eq!(OS_NAME, "Linux");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn os_release_does_not_call_this_machine_a_mac() {
        let name = os_release_name().expect("PRETTY_NAME");
        assert!(!name.is_empty());
        assert!(!claims_to_be_a_mac(&name), "{name}");
    }
}
