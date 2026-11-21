//! The quick actions a launcher offers without opening the window: the taskbar's jump list on
//! Windows (gpui writes it from the menu set with `set_dock_menu`), the Dock menu on macOS (the
//! same call) and a desktop file's `Actions` on Linux (`packaging/linux/dev.tinyclient.app.desktop`).
//! The list is static and short: per-conversation entries would show names on a shared screen.
//!
//! A second launch carries the action as an argument — `--action <name>` from a desktop action,
//! `--dock-action <index>` from the jump list gpui builds. As an invite link does, it rides the
//! single-instance handoff (`invite_ipc`) to the running window, and the second process exits.

/// The actions, in the order every launcher shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickAction {
    /// The File menu's New Message (⌘N): the composer's "to whom" switcher.
    NewMessage,
    /// The View menu's Inbox (⌘1).
    Inbox,
    /// The app menu's Quit tinyclient (⌘Q).
    Quit,
}

/// The whole list, for what registers it in one go.
pub const ALL: [QuickAction; 3] = [QuickAction::NewMessage, QuickAction::Inbox, QuickAction::Quit];

impl QuickAction {
    /// The name an argument and a handoff link carry.
    pub fn name(self) -> &'static str {
        match self {
            QuickAction::NewMessage => "new-message",
            QuickAction::Inbox => "inbox",
            QuickAction::Quit => "quit",
        }
    }

    /// The action named `name`, if it is one of the list.
    pub fn parse_name(name: &str) -> Option<Self> {
        ALL.into_iter().find(|action| action.name() == name)
    }

    /// Its place in the Windows jump list, whose items launch `--dock-action <index>`.
    pub fn index(self) -> usize {
        ALL.iter().position(|action| *action == self).unwrap_or(0)
    }

    /// The action at a jump list's index, if there is one.
    pub fn from_index(index: usize) -> Option<Self> {
        ALL.get(index).copied()
    }

    /// How the action travels the invite handoff to a running window.
    pub fn link(self) -> String {
        format!("tinyclient://action/{}", self.name())
    }
}

/// The action a handoff link carries, if it is one.
pub fn parse_link(link: &str) -> Option<QuickAction> {
    link.strip_prefix("tinyclient://action/").and_then(QuickAction::parse_name)
}

/// The quick action a launch asked for, if it did. `--action` is what the desktop file uses;
/// `--dock-action` is what the Windows jump list launches. An unknown name or index is not one.
pub fn launch_action(args: &[String]) -> Option<QuickAction> {
    let value = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1));
    if let Some(name) = value("--action") {
        return QuickAction::parse_name(name);
    }
    QuickAction::from_index(value("--dock-action")?.parse().ok()?)
}

/// What the single-instance handoff accepts: an invite link, a room link, a message link, or a quick action's.
pub fn handoff(payload: &str) -> bool {
    parse_link(payload).is_some()
        || (payload.starts_with("tinyclient://join#") && protocol::parse_invite_link(payload).is_some())
        || protocol::parse_room_link(payload).is_some()
        || protocol::parse_message_link(payload).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn arguments_name_an_action() {
        assert_eq!(launch_action(&args(&["--action", "new-message"])), Some(QuickAction::NewMessage));
        assert_eq!(launch_action(&args(&["--action", "inbox"])), Some(QuickAction::Inbox));
        assert_eq!(launch_action(&args(&["--action", "quit"])), Some(QuickAction::Quit));
        // The Windows jump list launches by index, in the list's order.
        for (index, action) in ALL.iter().enumerate() {
            assert_eq!(launch_action(&args(&["--dock-action", &index.to_string()])), Some(*action));
            assert_eq!((*action).index(), index);
        }
        assert_eq!(launch_action(&args(&["--action", "nope"])), None);
        assert_eq!(launch_action(&args(&["--action"])), None);
        assert_eq!(launch_action(&args(&["--dock-action", "9"])), None);
        assert_eq!(launch_action(&args(&["--dock-action", "-1"])), None);
        assert_eq!(launch_action(&args(&["tinyclient://join#abc"])), None);
        assert_eq!(launch_action(&[]), None);
    }

    #[test]
    fn links_round_trip() {
        for action in ALL {
            assert_eq!(parse_link(action.link().as_str()), Some(action));
        }
        assert_eq!(parse_link("tinyclient://action/"), None);
        assert_eq!(parse_link("tinyclient://action/nope"), None);
        assert_eq!(parse_link("tinyclient://join#abc"), None);
        assert_eq!(parse_link("hello"), None);
    }

    #[test]
    fn the_handoff_takes_invites_and_actions_only() {
        let url = protocol::invite_url("https://example.org", "token").unwrap();
        let invite = url.replacen("https://tinyclient.io/invite#", "tinyclient://join#", 1);
        assert!(handoff(&invite));
        assert!(handoff(&protocol::room_link("https://example.org", "lounge").unwrap()));
        assert!(handoff(&QuickAction::NewMessage.link()));
        assert!(!handoff("tinyclient://join#broken"));
        assert!(!handoff("tinyclient://room?server=example.org&id=.."));
        assert!(!handoff("tinyclient://action/nope"));
        assert!(!handoff("hello"));
    }

    #[test]
    fn the_desktop_file_declares_the_list_the_app_knows() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/dev.tinyclient.app.desktop");
        let text = std::fs::read_to_string(path).unwrap();
        let mut section = String::new();
        let mut values: Vec<(String, String, String)> = Vec::new();
        for line in text.lines() {
            if let Some(name) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
                section = name.to_owned();
            } else if let Some((key, value)) = line.split_once('=') {
                values.push((section.clone(), key.trim().to_owned(), value.trim().to_owned()));
            }
        }
        let listed = values
            .iter()
            .find(|(at, key, _)| at == "Desktop Entry" && key == "Actions")
            .map(|(_, _, value)| value)
            .expect("the desktop entry lists Actions");
        // Every action it lists is defined, launches with an argument the app maps back to
        // itself, and the whole list is the app's list in its order.
        for name in listed.split(';').filter(|name| !name.is_empty()) {
            let exec = values
                .iter()
                .find(|(at, key, _)| *at == format!("Desktop Action {name}") && key == "Exec")
                .map(|(_, _, value)| value)
                .unwrap_or_else(|| panic!("[Desktop Action {name}] has no Exec"));
            let words: Vec<String> = exec.split_whitespace().skip(1).map(String::from).collect();
            assert_eq!(launch_action(&words).map(|action| action.name()), Some(name), "`{exec}` asks for {name}");
        }
        let expected: Vec<&str> = ALL.iter().map(|action| action.name()).collect();
        assert_eq!(listed.split(';').filter(|name| !name.is_empty()).collect::<Vec<_>>(), expected);
    }
}
