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
    row("G4", General, "", "Install updates automatically", &[], &["update", "upgrade", "version", "install", "automatic"]),
    row("N1", Notifications, "", "Show notifications", &[], &["notify", "notification", "alert", "banner", "toast", "popup"]),
    row("N2", Notifications, "", "Notify me about", &["Direct messages and mentions", "All messages"], &["dm", "mention", "every message", "channel"]),
    row("ND", Notifications, "Notify for", "Direct messages", &[], &["dm", "private", "notify"]),
    row("NG", Notifications, "Notify for", "Group chats", &[], &["group", "notify"]),
    row("NM", Notifications, "Notify for", "Mentions in channels", &[], &["mention", "@", "everyone", "here", "notify"]),
    row("NT", Notifications, "Notify for", "Tiny servers", &[], &["tiny", "server", "room", "notify"]),
    row("N4", Notifications, "Privacy", "Show in notifications", &["Sender and message", "Sender only", "Nothing"], &["preview", "privacy", "screen sharing", "hide", "text"]),
    row("NL", Notifications, "Privacy", "When the screen is locked", &["Sender and message", "Sender only", "Nothing"], &["lock", "locked", "lock screen", "privacy"]),
    row("N5", Notifications, "Calls", "Incoming calls", &["Ring", "Silently", "Don't notify"], &["ring", "ringtone", "call", "phone"]),
    row("N6", Notifications, "Calls", "During a call", &["Banners only", "Banners and sounds", "Nothing"], &["call", "meeting", "busy"]),
    row("N7", Notifications, "Quiet time", "Pause notifications", &["For 1 hour", "Until tomorrow", "Until I resume"], &["pause", "snooze", "dnd", "do not disturb", "mute", "silence"]),
    row("N8", Notifications, "Quiet time", "Quiet hours", &[], &["quiet", "night", "sleep", "schedule", "dnd", "do not disturb"]),
    row("N9", Notifications, "Quiet time", "Let calls ring during quiet hours and pauses", &[], &["call", "ring", "quiet", "pause"]),
    row("N10", Notifications, "Attention", crate::host::FLASH_LABEL, &[], &["flash", "bounce", "dock", "taskbar", "attention"]),
    row("N11", Notifications, "Attention", crate::host::BADGE_LABEL, &[], &["badge", "count", "number", "unread", "dock", "taskbar"]),
    row("N13", Notifications, "Test", "Send a test notification", &[], &["test", "try", "check", "preview"]),
    row("S1", Sounds, "", "Play sounds", &[], &["sound", "audio", "mute", "silent", "ding", "chime", "beep"]),
    row("S2", Sounds, "Notification sounds", "Message sound", &["Pebble", "Drop", "Tap", "Chime", "Glass", "None"], &["ding", "chime", "tone", "sound for messages", "message sound"]),
    row("S3", Sounds, "Notification sounds", "Mention sound", &["Same", "Pebble", "Drop", "Tap", "Chime", "Glass", "None"], &["ding", "mention", "tone"]),
    row("S4", Sounds, "Notification sounds", "Ring", &["Soft ring", "Classic ring", "None"], &["ringtone", "call", "phone"]),
    row("SV", Sounds, "Notification sounds", "Notification volume", &[], &["volume", "loud", "quiet", "sound"]),
    row("A2", Appearance, "", "Text size", &["13 pt", "14 pt", "15 pt", "16 pt", "17 pt"], &["font", "bigger", "smaller", "zoom", "large"]),
    row("A3", Appearance, "", "Density", &["Comfortable", "Compact"], &["rows", "dense", "spacing", "compact"]),
    row("A4", Appearance, "", "Show message previews in the list", &[], &["preview", "screen sharing", "privacy", "list"]),
    row("M1", Messages, "", "Send with", &["Return", "⌘Return"], &["enter", "return", "newline", "send key", "cmd"]),
    row("M2", Messages, "", "Open conversations at", &["First unread", "Newest message"], &["scroll", "unread", "bottom", "jump"]),
    row("M3", Messages, "", "Show when others are typing", &[], &["typing", "indicator"]),
    row("M4", Messages, "", "Convert emoticons to emoji", &[], &["smiley", "emoji", "emoticon", ":)"]),
    row("M5", Messages, "", "Play GIFs and animated stickers", &["Automatically", "On hover", "Never"], &["gif", "animated", "autoplay", "sticker", "motion", "animation"]),
    row("P1", Privacy, "Let others see when I am typing", "Let others see when I am typing", &[], &["typing", "indicator", "privacy"]),
    row("P2", Privacy, "Let others see what I have read", "Let others see what I have read", &[], &["receipts", "read receipts", "seen", "privacy"]),
    row("P3", Privacy, "", "Load images and link previews automatically", &[], &["images", "pictures", "previews", "data", "tracking"]),
    row("P4", Privacy, "", crate::host::KEEP_MESSAGES, &["7 days", "30 days", "90 days"], &["retention", "history", "delete", "days"]),
    row("V1", Audio, "Microphone", "Microphone", &[], &["input", "mic", "device", "headset"]),
    row("V2", Audio, "Speaker", "Speaker", &[], &["output", "headphones", "device", "speakers"]),
    row("V5", Audio, "Speaker", "Call volume", &[], &["volume", "loud", "quiet", "voices"]),
    row("V4", Audio, "Joining", "Join calls with my microphone muted", &[], &["mute", "muted", "join"]),
    row("V8", Voice, "", "Microphone opens by", &["Voice activation", "Push to talk"], &["ptt", "push to talk", "voice activity", "vad", "open mic"]),
    row("V3", Voice, "", "Input level", &[], &["test", "meter", "mic", "level"]),
    row("V6", Voice, "", "Sensitivity", &[], &["gate", "threshold", "noise", "voice activity", "vad"]),
    row("V18", Voice, "", "Hear yourself while testing", &[], &["loopback", "test", "listen", "monitor", "echo test"]),
    row("V19", Voice, "", "Tell me when I talk while muted", &[], &["muted", "talking", "warning", "mute", "speak"]),
    row("V14", Voice, "", "Release delay", &["0 ms", "100 ms", "200 ms", "300 ms", "500 ms", "1 s"], &["ptt", "push to talk", "delay", "hold", "cut off"]),
    row("V13", Voice, "Keys", "Push to talk", &[], &["ptt", "key", "hotkey", "shortcut", "global", "mouse"]),
    row("V15", Voice, "Keys", "Priority push to talk", &[], &["priority", "speaker", "ptt", "hotkey", "global"]),
    row("V16", Voice, "Keys", "Toggle mute", &[], &["mute", "hotkey", "shortcut", "global"]),
    row("V17", Voice, "Keys", "Toggle deafen", &[], &["deafen", "hotkey", "shortcut", "global"]),
    row("V10", Voice, "Processing", "Echo cancellation", &[], &["echo", "aec", "speakers", "feedback"]),
    row("V11", Voice, "Processing", "Noise suppression", &[], &["noise", "background", "fan", "keyboard", "denoise"]),
    row("V12", Voice, "Processing", "Automatic gain", &[], &["agc", "gain", "loudness", "quiet", "volume"]),
    row("S7", Voice, "Call sounds", "Someone joins, leaves or I'm moved", &[], &["sound", "chime", "ding", "join", "leave", "move", "cue"]),
    row("S8", Voice, "Call sounds", "I mute, unmute or deafen", &[], &["sound", "ding", "tick", "cue", "deafen"]),
    row("S9", Voice, "Call sounds", "Connection lost, back or ended", &[], &["sound", "ding", "cue", "disconnect", "reconnect", "kicked"]),
    row("S10", Voice, "Call sounds", "Call sound volume", &[], &["volume", "loud", "quiet", "sound", "test sound", "play"]),
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
    ("reaction", "tinyclient does not notify about reactions yet."),
    ("custom sound", "tinyclient plays only its own sounds for now."),
    ("sound file", "tinyclient plays only its own sounds for now."),
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
    ("menu bar", crate::host::NO_TRAY_ICON),
    ("accent", "tinyclient keeps one accent colour."),
    ("font", "tinyclient keeps the system font. Text size is in Appearance."),
    ("language", crate::host::FOLLOWS_LANGUAGE),
    ("sync", crate::host::SETTINGS_STAY),
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
    let shown = ROWS.iter().enumerate().filter(|(_, row)| !absent(row.id));
    let mut found: Vec<(u8, usize, &Row)> = shown.filter_map(|(at, row)| rank(row).map(|rank| (rank, at, row))).collect();
    found.sort_by_key(|(rank, at, _)| (*rank, *at));
    found.into_iter().map(|(_, _, row)| row).collect()
}

/// Rows this system cannot honour, so they are neither drawn nor found here: macOS plays
/// sounds at its own alert volume and hides previews on a locked screen itself; Linux has no
/// badge or flash here.
pub fn absent(id: &str) -> bool {
    match id {
        "G2" => !cfg!(target_os = "macos"),
        "SV" | "NL" => cfg!(target_os = "macos"),
        "N10" | "N11" => !cfg!(any(windows, target_os = "macos")),
        _ => false,
    }
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
    matches!(
        id,
        "N1" | "N2" | "ND" | "NG" | "NM" | "NT" | "N4" | "NL" | "N5" | "N6" | "N7" | "N8" | "N9" | "N10" | "N11" | "N13" | "S2" | "S3" | "S4" | "SV"
            | "G2" | "S1" | "S7" | "S8" | "S9" | "S10" | "P1" | "P2" | "P4" | "V1" | "V2" | "V3" | "V4" | "V5" | "V6" | "V8" | "V10" | "V11" | "V12" | "V13" | "V14" | "V15" | "V16" | "V17" | "V19" | "D1" | "D2" | "D3"
    )
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
    key("Edit", "Search in Conversation", "⌘F"),
    key("Edit", "Search Everywhere", "⇧⌘F"),
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

/// Hiding an application is macOS's: GPUI ignores it on Linux and panics at Hide Others on
/// Windows, so these keys are bound and listed on macOS only (`menubar::bind_keys`).
const MAC_ONLY: &[&str] = &["Hide tinyclient", "Hide Others"];

/// A rail place after Inbox and People, with the digit key the View menu gives it.
pub struct PlaceKey {
    pub command: String,
    pub keys: &'static str,
}

const PLACE_KEYS: [&str; 7] = ["⌘3", "⌘4", "⌘5", "⌘6", "⌘7", "⌘8", "⌘9"];

pub fn place_keys(names: &[String]) -> Vec<PlaceKey> {
    names.iter().take(PLACE_KEYS.len()).enumerate().map(|(ix, name)| PlaceKey { command: name.clone(), keys: PLACE_KEYS[ix] }).collect()
}

pub fn shortcuts(query: &str) -> Vec<&'static Shortcut> {
    let query = fold(query.trim());
    SHORTCUTS
        .iter()
        .filter(|s| cfg!(target_os = "macos") || !MAC_ONLY.contains(&s.command))
        .filter(|s| query.is_empty() || at_word_start(s.command, &query) || at_word_start(s.menu, &query))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_finds_by_label_value_and_keyword_and_ranks_the_label_first() {
        let ids = |query: &str| search(query).into_iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids("text size")[0], "A2");
        assert!(ids("ding").starts_with(&["S1", "S2", "S3", "S7", "S8", "S9"]));
        assert_eq!(ids("push to talk")[0], "V13");
        assert!(ids("push to talk").contains(&"V8"));
        assert!(ids("echo").contains(&"V10"));
        assert!(ids("Enter").contains(&"M1"));
        assert!(ids("receipts").contains(&"P2"));
        assert!(ids("cache").contains(&"D2"));
        assert_eq!(ids("compact"), ["A3"]);
        assert_eq!((ids("GIF"), not_yet("gif")), (vec!["M5"], None));
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
        // The words people reach for, not only the names in the product doc.
        for words in ["launch", "start at login", "autostart", "reactions", "custom sound"] {
            assert!(search(words).is_empty(), "{words} finds a row");
            assert!(not_yet(words).is_some(), "{words} gets no line");
        }
        assert_eq!(not_yet("zzz"), None);
    }

    #[test]
    fn notifications_and_their_sounds_are_found_by_the_words_people_use() {
        let ids = |query: &str| search(query).into_iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids("Notifications")[0], "N1");
        let asked = [("notify", "N1"), ("alert", "N1"), ("dnd", "N7"), ("do not disturb", "N7"), ("snooze", "N7"), ("quiet hours", "N8"), ("message sound", "S2"), ("ringtone", "S4"), ("lock screen", "NL"), ("test notification", "N13")];
        for (words, id) in asked {
            if absent(id) {
                continue;
            }
            assert!(ids(words).contains(&id), "{words} does not find {id}");
            assert_eq!(not_yet(words), None, "{words} is built");
        }
        assert_eq!(ids("badge").contains(&"N11"), !absent("N11"));
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
    fn hiding_the_app_is_listed_only_where_it_does_something() {
        let listed = shortcuts("").iter().filter(|s| MAC_ONLY.contains(&s.command)).count();
        assert_eq!(listed, if cfg!(target_os = "macos") { MAC_ONLY.len() } else { 0 });
        assert!(shortcuts("").iter().any(|s| s.command == "Quit tinyclient"));
        assert_eq!(shortcuts("hide others").is_empty(), !cfg!(target_os = "macos"));
    }

    #[test]
    fn rail_places_after_inbox_and_people_keep_their_digit() {
        let rows = place_keys(&["Discord".into(), "Slack".into()]);
        assert_eq!((rows[0].command.as_str(), rows[0].keys), ("Discord", "⌘3"));
        assert_eq!((rows[1].command.as_str(), rows[1].keys), ("Slack", "⌘4"));
        assert!(search("dnd").iter().any(|row| row.id == "N7"));
    }

    #[test]
    fn keep_receiving_is_listed_only_where_closing_the_window_can_hide_the_app() {
        assert_eq!(search("keep receiving").iter().any(|row| row.id == "G2"), cfg!(target_os = "macos"));
    }

    #[test]
    fn every_shortcut_names_a_menu_item_that_exists_and_every_keyed_item_is_listed() {
        use crate::gui::app::menubar::menus;
        use gpui::MenuItem;
        // A submenu's items belong to their parent menu, as they are drawn.
        fn names(item: &MenuItem) -> impl Iterator<Item = &str> {
            match item {
                MenuItem::Action { name, .. } => Box::new(std::iter::once(name.as_ref())) as Box<dyn Iterator<Item = &str>>,
                MenuItem::Submenu(menu) => Box::new(menu.items.iter().flat_map(names)) as Box<dyn Iterator<Item = &str>>,
                _ => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &str>>,
            }
        }
        let bar = menus(&[], None, "Check for Updates…");
        for shortcut in SHORTCUTS.iter().filter(|s| s.menu != "Calls" && (cfg!(target_os = "macos") || !MAC_ONLY.contains(&s.command))) {
            let menu = bar.iter().find(|menu| menu.name.as_ref() == shortcut.menu).unwrap_or_else(|| panic!("no {} menu", shortcut.menu));
            let found = menu.items.iter().any(|item| names(item).any(|name| name == shortcut.command));
            assert!(found, "{} is not in {}", shortcut.command, shortcut.menu);
        }
        // The menu items bound in `menubar::bind_keys` and `gui::bind_keys`.
        for (menu, command) in [("tinyclient", "Settings…"), ("File", "New Message"), ("File", "Close Window"), ("View", "Bigger Text"), ("Help", "Keyboard Shortcuts"), ("Conversation", "Start Call")] {
            assert!(SHORTCUTS.iter().any(|s| s.menu == menu && s.command == command), "{command} is missing");
        }
    }
}
