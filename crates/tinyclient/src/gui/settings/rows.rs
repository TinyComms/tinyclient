//! The one table of every row in Settings (`docs/design/09-settings.md`, GPUI notes): its id
//! from the product doc, its section and group, its label, and what search matches it by.
//! Drawing, search and the accessibility labels all read it. A row whose feature is not in this
//! build is not in the table, which is how "absent, not greyed" holds.

use super::Section;

pub struct Row {
    /// The product doc's id: G2, S7, A2.
    pub id: &'static str,
    pub section: Section,
    /// The group label it sits under, empty for the first group.
    pub group: &'static str,
    pub label: &'static str,
    /// The names of its choices, as shown.
    pub values: &'static [&'static str],
    /// Other words people use for it.
    pub keywords: &'static [&'static str],
}

const fn row(
    id: &'static str,
    section: Section,
    group: &'static str,
    label: &'static str,
    values: &'static [&'static str],
    keywords: &'static [&'static str],
) -> Row {
    Row { id, section, group, label, values, keywords }
}

use Section::*;

/// Every row, in the order the sections draw them.
pub const ROWS: &[Row] = &[
    row("G2", General, "", "Keep receiving when the window is closed", &[], &["background", "quit", "close", "run", "daemon"]),
    row("S1", Sounds, "", "Play sounds", &[], &["sound", "audio", "mute", "silent", "ding", "chime", "beep"]),
    row("S7", Sounds, "In calls", "Someone joins or leaves", &[], &["sound", "chime", "ding", "join", "leave", "cue"]),
    row("S8", Sounds, "In calls", "I mute, unmute or deafen", &[], &["sound", "ding", "tick", "cue", "deafen"]),
    row("S9", Sounds, "In calls", "Connection lost or back", &[], &["sound", "ding", "cue", "disconnect", "reconnect"]),
    row("S10", Sounds, "In calls", "Call sound volume", &[], &["volume", "loud", "quiet", "sound", "test sound", "play"]),
    row("A2", Appearance, "", "Text size", &["13 pt", "14 pt", "15 pt", "16 pt", "17 pt"], &["font", "bigger", "smaller", "zoom", "large"]),
    row("A3", Appearance, "", "Density", &["Comfortable", "Compact"], &["rows", "dense", "spacing", "compact"]),
    row("A4", Appearance, "", "Show message previews in the list", &[], &["preview", "screen sharing", "privacy", "list"]),
    row("M1", Messages, "", "Send with", &["Return", "⌘Return"], &["enter", "return", "newline", "send key", "cmd"]),
    row("M2", Messages, "", "Open conversations at", &["First unread", "Newest message"], &["scroll", "unread", "bottom", "jump"]),
    row("M3", Messages, "", "Show when others are typing", &[], &["typing", "indicator"]),
    row("M4", Messages, "", "Convert emoticons to emoji", &[], &["smiley", "emoji", "emoticon", ":)"]),
    row("P1", Privacy, "Let others see when I am typing", "Let others see when I am typing", &[], &["typing", "indicator", "privacy"]),
    row("P2", Privacy, "Let others see what I have read", "Let others see what I have read", &[], &["receipts", "read receipts", "seen", "privacy"]),
    row("P3", Privacy, "", "Load images and link previews automatically", &[], &["images", "pictures", "previews", "data", "tracking"]),
    row("P4", Privacy, "", "Keep messages on this Mac for", &["7 days", "30 days", "90 days"], &["retention", "history", "delete", "days"]),
    row("V1", Audio, "Microphone", "Microphone", &[], &["input", "mic", "device", "headset"]),
    row("V3", Audio, "Microphone", "Input level", &[], &["test", "meter", "mic", "level"]),
    row("V6", Audio, "Microphone", "Sensitivity", &[], &["gate", "threshold", "noise", "voice activity"]),
    row("V2", Audio, "Speaker", "Speaker", &[], &["output", "headphones", "device", "speakers"]),
    row("V5", Audio, "Speaker", "Call volume", &[], &["volume", "loud", "quiet", "voices"]),
    row("V4", Audio, "Joining", "Join calls with my microphone muted", &[], &["mute", "muted", "join"]),
    row("D1", Storage, "", "Storage used", &[], &["disk", "space", "size", "cache", "usage"]),
    row("D2", Storage, "", "Keep images and files up to", &["250 MB", "500 MB", "1 GB", "2 GB"], &["cache", "disk", "space", "limit"]),
    row("D3", Storage, "", "Clear saved messages and files", &[], &["clear", "delete", "cache", "free space"]),
    row("B1", About, "", "Copy diagnostics", &[], &["support", "bug", "report", "version"]),
    row("B2", About, "", "Logs", &[], &["log", "debug", "support"]),
    row("B3", About, "", "Reset all settings", &[], &["reset", "defaults", "factory"]),
];

/// Words for what tinyclient does not have yet, or will not have, so a search for one gets an
/// honest line rather than nothing.
pub const NOT_YET: &[(&str, &str)] = &[
    ("proxy", "tinyclient has no proxy setting yet."),
    ("socks", "tinyclient has no proxy setting yet."),
    ("message sound", "tinyclient plays no sound for messages yet. It comes with notifications."),
    ("sound for messages", "tinyclient plays no sound for messages yet. It comes with notifications."),
    ("notification", "tinyclient does not post notifications yet."),
    ("notify", "tinyclient does not post notifications yet."),
    ("alert", "tinyclient does not post notifications yet."),
    ("dnd", "tinyclient does not post notifications yet. Do Not Disturb you set in Slack or Discord is shown on your status."),
    ("focus", "tinyclient does not post notifications yet, so a Focus has nothing to hold back."),
    ("banner", "tinyclient does not post notifications yet."),
    ("do not disturb", "tinyclient does not post notifications yet. Do Not Disturb you set in Slack or Discord is shown on your status."),
    ("quiet hours", "tinyclient does not post notifications yet."),
    ("badge", "tinyclient has no Dock badge setting yet."),
    ("ringtone", "tinyclient has no ring sound yet; calls ring in the window."),
    ("login", "tinyclient has no open-at-login setting yet."),
    ("startup", "tinyclient has no open-at-login setting yet."),
    ("launch", "tinyclient has no open-at-login setting yet."),
    ("autostart", "tinyclient has no open-at-login setting yet."),
    ("theme", "tinyclient is dark only for now."),
    ("light", "tinyclient is dark only for now."),
    ("dark mode", "tinyclient is dark only for now."),
    ("spell", "tinyclient has no spell checking yet."),
    ("touch id", "tinyclient has no lock yet."),
    ("camera", "tinyclient has no camera yet."),
    ("push to talk", "tinyclient has no push to talk yet."),
    ("gif", "tinyclient has no GIF playback setting yet."),
    ("menu bar", "tinyclient has no menu bar icon yet."),
    ("update", "tinyclient has no updater yet."),
    ("accent", "tinyclient keeps one accent colour."),
    ("font", "tinyclient keeps the system font. Text size is in Appearance."),
    ("language", "tinyclient follows the language of macOS."),
    ("sync", "Settings stay on this Mac. Nothing is synced."),
];

/// Lower case with the accents taken off, so "Résumé" finds "resume".
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

/// Whether `query` starts a word of `text`, or the whole query is in it from a word start.
fn at_word_start(text: &str, query: &str) -> bool {
    let text = fold(text);
    text.match_indices(query).any(|(at, _)| at == 0 || !text[..at].chars().next_back().is_some_and(char::is_alphanumeric))
}

/// The rows that match, best first: by label, then values, then keywords, then section name.
pub fn search(query: &str) -> Vec<&'static Row> {
    let query = fold(query.trim());
    if query.is_empty() {
        return Vec::new();
    }
    let rank = |row: &Row| -> Option<u8> {
        if at_word_start(row.label, &query) || at_word_start(row.group, &query) {
            Some(0)
        } else if row.values.iter().any(|value| at_word_start(value, &query)) {
            Some(1)
        } else if row.keywords.iter().any(|word| at_word_start(word, &query)) {
            Some(2)
        } else if at_word_start(row.section.name(), &query) {
            Some(3)
        } else {
            None
        }
    };
    let mut found: Vec<(u8, usize, &Row)> = ROWS.iter().enumerate().filter_map(|(at, row)| rank(row).map(|rank| (rank, at, row))).collect();
    found.sort_by_key(|(rank, at, _)| (*rank, *at));
    found.into_iter().map(|(_, _, row)| row).collect()
}

/// The honest line for a search that found nothing, when it names something tinyclient lacks.
pub fn not_yet(query: &str) -> Option<&'static str> {
    let query = fold(query.trim());
    NOT_YET.iter().find(|(term, _)| query.contains(term) || (query.len() >= 3 && term.starts_with(&query))).map(|(_, words)| *words)
}

/// A search result's breadcrumb: its section, and its group when it has one. P1 and P2 are a
/// card of accounts under their group, so the group is what names the setting there.
pub fn crumb(row: &Row) -> String {
    if row.group.is_empty() {
        row.section.name().to_owned()
    } else {
        format!("{} › {}", row.section.name(), row.group)
    }
}

/// Whether a row draws its own label. P1 and P2 draw one line per account instead.
pub fn draws_label(id: &str) -> bool {
    !matches!(id, "P1" | "P2")
}

/// The rows whose value chatd keeps or whose action chatd carries out: drawn disabled, and
/// taking no change, while chatd cannot be reached.
pub fn kept_by_chatd(id: &str) -> bool {
    matches!(id, "G2" | "S1" | "S7" | "S8" | "S9" | "S10" | "P1" | "P2" | "P4" | "V1" | "V2" | "V3" | "V4" | "V5" | "V6" | "D1" | "D2" | "D3")
}

pub fn find(id: &str) -> Option<&'static Row> {
    ROWS.iter().find(|row| row.id == id)
}

/// One line of the Shortcuts section: the menu it is in, the command and its keys.
pub struct Shortcut {
    pub menu: &'static str,
    pub command: &'static str,
    pub keys: &'static str,
}

const fn key(menu: &'static str, command: &'static str, keys: &'static str) -> Shortcut {
    Shortcut { menu, command, keys }
}

/// Every key, grouped as the menu bar is. The test below holds it to the menus, so it cannot
/// name a command the bar does not have. The call keys are in no menu.
pub const SHORTCUTS: &[Shortcut] = &[
    key("tinyclient", "Settings…", "⌘,"),
    key("tinyclient", "Hide tinyclient", "⌘H"),
    key("tinyclient", "Hide Others", "⌥⌘H"),
    key("tinyclient", "Quit tinyclient", "⌘Q"),
    key("File", "New Message", "⌘N"),
    key("File", "Attach File…", "⌘U"),
    key("File", "Close Window", "⌘W"),
    key("Edit", "Undo", "⌘Z"),
    key("Edit", "Redo", "⇧⌘Z"),
    key("Edit", "Cut", "⌘X"),
    key("Edit", "Copy", "⌘C"),
    key("Edit", "Paste", "⌘V"),
    key("Edit", "Select All", "⌘A"),
    key("View", "Inbox", "⌘1"),
    key("View", "People", "⌘2"),
    key("View", "Quick Switcher", "⌘K"),
    key("View", "Hide Sidebar", "⌘\\"),
    key("View", "Close Right Pane", "⌘."),
    key("View", "Bigger Text", "⌘+"),
    key("View", "Smaller Text", "⌘−"),
    key("View", "Actual Size", "⌘0"),
    key("View", "Enter Full Screen", "⌃⌘F"),
    key("Conversation", "Back", "⌘["),
    key("Conversation", "Forward", "⌘]"),
    key("Conversation", "Up a Level", "⌘↑"),
    key("Conversation", "Next", "⌥↓"),
    key("Conversation", "Previous", "⌥↑"),
    key("Conversation", "Next Unread", "⌥⇧↓"),
    key("Conversation", "Previous Unread", "⌥⇧↑"),
    key("Conversation", "Mark as Read", "⇧Esc"),
    key("Conversation", "Show Details", "⌥⌘I"),
    key("Conversation", "Start Call", "⇧⌘C"),
    key("Conversation", "Open in App", "⌥⌘O"),
    key("Window", "Minimize", "⌘M"),
    key("Help", "Keyboard Shortcuts", "⌘/"),
    key("Calls", "Mute or unmute", "⇧⌘M"),
    key("Calls", "Deafen or undeafen", "⇧⌘D"),
    key("Calls", "Leave the call", "⇧⌘H"),
    key("Calls", "Accept a call", "⇧⌘A"),
    key("Calls", "Decline a call", "⇧⌘R"),
    key("Calls", "Go to the call", "⇧⌘J"),
];

pub fn shortcuts(query: &str) -> Vec<&'static Shortcut> {
    let query = fold(query.trim());
    SHORTCUTS.iter().filter(|s| query.is_empty() || at_word_start(s.command, &query) || at_word_start(s.menu, &query)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_finds_by_label_value_and_keyword_and_ranks_the_label_first() {
        let ids = |query: &str| search(query).into_iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids("text size")[0], "A2");
        assert!(ids("ding").starts_with(&["S1", "S7", "S8", "S9"]));
        assert!(ids("Enter").contains(&"M1"));
        assert!(ids("receipts").contains(&"P2"));
        assert!(ids("cache").contains(&"D2"));
        assert_eq!(ids("compact"), ["A3"]);
        // At word starts only: "ize" is in "size" and finds nothing.
        assert!(ids("ize").is_empty());
        assert!(ids("   ").is_empty());
        // Accents and case do not matter.
        assert_eq!(ids("DÉNSITY"), ids("density"));
    }

    #[test]
    fn every_result_says_which_setting_it_is() {
        // A result is its breadcrumb over its live row: one of the two names the setting, so a
        // card of account switches is never shown without saying what the switches do.
        for row in ROWS {
            assert!(crumb(row).contains(row.label) || draws_label(row.id), "{} has no name in its result", row.id);
        }
        assert_eq!(crumb(find("P1").unwrap()), "Privacy › Let others see when I am typing");
        assert_eq!(crumb(find("G2").unwrap()), "General");
    }

    #[test]
    fn a_search_for_what_is_not_built_says_so() {
        assert!(search("proxy").is_empty());
        assert_eq!(not_yet("proxy"), Some("tinyclient has no proxy setting yet."));
        assert!(not_yet("Notifications").is_some());
        // The words people reach for, not only the names in the product doc.
        for words in ["notify", "alert", "dnd", "focus", "launch", "start at login", "autostart", "sound for messages", "message sound"] {
            assert!(search(words).is_empty(), "{words} finds a row");
            assert!(not_yet(words).is_some(), "{words} gets no line");
        }
        assert_eq!(not_yet("zzz"), None);
    }

    #[test]
    fn every_row_is_named_once_and_every_section_but_accounts_and_shortcuts_has_rows() {
        let mut ids: Vec<&str> = ROWS.iter().map(|row| row.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ROWS.len());
        for section in Section::ALL {
            let rows = ROWS.iter().filter(|row| row.section == section).count();
            assert_eq!(rows == 0, matches!(section, Section::Accounts | Section::Shortcuts), "{section:?}");
        }
    }

    #[test]
    fn every_shortcut_names_a_menu_item_that_exists_and_every_keyed_item_is_listed() {
        use crate::gui::app::menubar::menus;
        use gpui::MenuItem;
        let bar = menus(&[], None);
        for shortcut in SHORTCUTS.iter().filter(|s| s.menu != "Calls") {
            let menu = bar.iter().find(|menu| menu.name.as_ref() == shortcut.menu).unwrap_or_else(|| panic!("no {} menu", shortcut.menu));
            let found = menu.items.iter().any(|item| match item {
                MenuItem::Action { name, .. } => name.as_ref() == shortcut.command,
                _ => false,
            });
            assert!(found, "{} is not in {}", shortcut.command, shortcut.menu);
        }
        // The menu items bound in `menubar::bind_keys` and `gui::bind_keys`.
        for (menu, command) in [("tinyclient", "Settings…"), ("File", "New Message"), ("File", "Close Window"), ("View", "Bigger Text"), ("Help", "Keyboard Shortcuts"), ("Conversation", "Start Call")] {
            assert!(SHORTCUTS.iter().any(|s| s.menu == menu && s.command == command), "{command} is missing");
        }
    }
}
