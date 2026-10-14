//! The menu bar (`polish/01-system.md` §6). Native menus; an item whose action nobody handles
//! where focus is shows disabled, never hidden. Names follow the open place ("Open in Slack"),
//! so the bar is set again when the open conversation or the rail's places change.
//!
// CONTRACT: these items have actions here but their handlers belong to other views, and stay
// disabled until those views register them with `.on_action`: Attach File (`AttachFile`,
// composer), Show Details (`ShowDetails`, pane), Mute… (`Mute`, profile), Start Call
// (`StartCall`, call) and Open in <network> (`OpenInNetwork`, handoff). Items with nothing built
// behind them are left out rather than shown dead: Check for Updates…, Find (⌘F, ⇧⌘F), Mark as
// Unread, Mark Server as Read, Copy Link, Release Notes and Report a Problem…. They come back
// with what they do.

use super::super::settings;
use super::super::sidebar::Mode;
use super::super::state::Shell;
use super::super::*;
use gpui::{App, Menu, MenuItem, OsAction, SystemMenuType};
use protocol::Network;

/// The menus for a window with `places` on the rail and `open_network` open.
pub fn menus(places: &[(String, usize)], open_network: Option<Network>) -> Vec<Menu> {
    let place = |name: String, index: usize| -> MenuItem {
        match index {
            0 => MenuItem::action(name, Place1),
            1 => MenuItem::action(name, Place2),
            2 => MenuItem::action(name, Place3),
            3 => MenuItem::action(name, Place4),
            4 => MenuItem::action(name, Place5),
            5 => MenuItem::action(name, Place6),
            6 => MenuItem::action(name, Place7),
            7 => MenuItem::action(name, Place8),
            _ => MenuItem::action(name, Place9),
        }
    };
    let open_in = match open_network {
        Some(network) => format!("Open in {}", network.label()),
        None => "Open in App".to_owned(),
    };
    let mut view = vec![MenuItem::action("Inbox", Place1), MenuItem::action("People", Place2)];
    view.extend(places.iter().filter(|(_, ix)| (2..9).contains(ix)).map(|(name, ix)| place(name.clone(), *ix)));
    view.extend([
        MenuItem::separator(),
        MenuItem::action("Quick Switcher", Switcher),
        MenuItem::action("Hide Sidebar", ToggleSidebar),
        MenuItem::action("Close Right Pane", CloseRight),
        MenuItem::separator(),
        MenuItem::action("Bigger Text", BiggerText),
        MenuItem::action("Smaller Text", SmallerText),
        MenuItem::action("Actual Size", ActualSize),
        MenuItem::separator(),
        MenuItem::action("Enter Full Screen", FullScreen),
    ]);
    vec![
        Menu::new("tinyclient").items([
            MenuItem::action("About tinyclient", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", Settings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide tinyclient", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit tinyclient", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Message", NewMessage),
            MenuItem::action("Add Account…", AddAccount),
            MenuItem::separator(),
            MenuItem::action("Attach File…", AttachFile),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::action("Paste and Match Style", input::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            // macOS adds Emoji & Symbols, Dictation and AutoFill itself.
        ]),
        Menu::new("View").items(view),
        Menu::new("Conversation").items([
            MenuItem::action("Back", Back),
            MenuItem::action("Forward", Forward),
            MenuItem::action("Up a Level", UpLevel),
            MenuItem::separator(),
            MenuItem::action("Next", NextRow),
            MenuItem::action("Previous", PreviousRow),
            MenuItem::action("Next Unread", NextUnread),
            MenuItem::action("Previous Unread", PreviousUnread),
            MenuItem::separator(),
            MenuItem::action("Mark as Read", MarkRead),
            MenuItem::separator(),
            MenuItem::action("Show Details", ShowDetails),
            MenuItem::action("Star", Star),
            MenuItem::action("Mute…", Mute),
            MenuItem::separator(),
            MenuItem::action("Start Call", StartCall),
            MenuItem::action(open_in, OpenInNetwork),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("tinyclient", ShowWindow),
            MenuItem::action("Bring All to Front", ShowWindow),
        ]),
        Menu::new("Help").items([
            MenuItem::action("Keyboard Shortcuts", KeyboardShortcuts),
            MenuItem::action("Copy Diagnostics", CopyDiagnostics),
        ]),
    ]
}

/// The keys of the menu bar's items. The rest are bound in `gui::bind_keys`.
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("cmd-,", Settings, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-n", NewMessage, None),
        KeyBinding::new("cmd-u", AttachFile, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-=", BiggerText, None),
        KeyBinding::new("cmd-+", BiggerText, None),
        KeyBinding::new("cmd--", SmallerText, None),
        KeyBinding::new("cmd-0", ActualSize, None),
        KeyBinding::new("ctrl-cmd-f", FullScreen, None),
        KeyBinding::new("alt-cmd-i", ShowDetails, None),
        KeyBinding::new("cmd-shift-c", StartCall, None),
        KeyBinding::new("alt-cmd-o", OpenInNetwork, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-/", KeyboardShortcuts, None),
    ]);
}

/// ⌘Q. chatd is stopped by `stop_chatd_on_quit`, as for every other way out.
pub fn quit(cx: &mut App) {
    cx.quit();
}

/// Quitting stops chatd too (`docs/product/09-settings.md` G2), however it happens: ⌘Q, Dock ›
/// Quit, logging out or shutting down all end in the app's quit handlers. Asked off the main
/// thread and briefly, so a chatd that does not answer cannot hold the quit; GPUI waits 200 ms
/// for the handlers, and the request is written well before that.
pub fn stop_chatd_on_quit(cx: &mut App) {
    if cfg!(feature = "gui-shot") {
        return;
    }
    cx.on_app_quit(|cx| {
        cx.background_executor().spawn(async {
            let _ = crate::client::ask_within(&protocol::Request::Shutdown, Some(std::time::Duration::from_millis(150)));
        })
    })
    .detach();
}

/// ⌘W and the red button: with Keep receiving on, the window hides and chatd goes on receiving
/// and the Dock badge counting; off, tinyclient quits.
pub fn close_window(cx: &mut App) {
    if super::prefs::values().on(protocol::settings::KEEP_RUNNING) {
        cx.hide();
    } else {
        quit(cx);
    }
}

/// The app-wide handlers: they act on the app, not on what has focus.
pub fn app_actions(cx: &mut App) {
    stop_chatd_on_quit(cx);
    cx.on_action(|_: &Quit, cx| quit(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CloseWindow, cx| close_window(cx));
    cx.on_action(|_: &ShowWindow, cx| cx.activate(true));
    cx.on_action(|_: &Settings, cx| settings::open(settings::Section::last(), cx));
    cx.on_action(|_: &About, cx| settings::open(settings::Section::About, cx));
    cx.on_action(|_: &KeyboardShortcuts, cx| settings::open(settings::Section::Shortcuts, cx));
}

impl Shell {
    /// Sets the menu bar again when what its names depend on changed. A compare when nothing did.
    pub(in crate::gui) fn refresh_menus(&self, cx: &mut gpui::Context<Self>) {
        let places: Vec<(String, usize)> = self
            .places()
            .into_iter()
            .enumerate()
            .map(|(ix, mode)| {
                let name = match &mode {
                    Mode::Inbox => "Inbox".to_owned(),
                    Mode::People => "People".to_owned(),
                    Mode::Network(network) => network.label().to_owned(),
                    Mode::Space(key) => self.spaces.iter().find(|s| &s.key == key).map_or_else(|| key.id.clone(), |s| s.name.clone()),
                };
                (name, ix)
            })
            .collect();
        let open = self.open.as_ref().map(|key| key.network);
        let signature = format!("{places:?}{open:?}");
        if *self.menus_set.borrow() == signature {
            return;
        }
        self.menus_set.replace(signature);
        cx.set_menus(menus(&places, open));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_has_every_menu_and_names_the_open_network() {
        let places = vec![("Inbox".to_owned(), 0), ("People".to_owned(), 1), ("Discord".to_owned(), 2), ("Moss & Lichen".to_owned(), 3)];
        let bar = menus(&places, Some(Network::Slack));
        let names: Vec<&str> = bar.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(names, ["tinyclient", "File", "Edit", "View", "Conversation", "Window", "Help"]);
        let items = |menu: &Menu| -> Vec<String> {
            menu.items
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Action { name, .. } => Some(name.to_string()),
                    _ => None,
                })
                .collect()
        };
        assert!(items(&bar[4]).contains(&"Open in Slack".to_owned()));
        // The rail's places after Inbox and People, in its order; no second Inbox.
        assert_eq!(items(&bar[3])[..4], ["Inbox", "People", "Discord", "Moss & Lichen"]);
        assert!(items(&bar[1]).contains(&"Close Window".to_owned()));
        // Nothing is offered that does nothing.
        let all: Vec<String> = bar.iter().flat_map(items).collect();
        for dead in ["Check for Updates…", "Search in Conversation", "Search Everywhere", "Release Notes", "Report a Problem…", "Mark as Unread", "Copy Link"] {
            assert!(!all.contains(&dead.to_owned()), "{dead} is offered");
        }
        assert!(all.contains(&"Settings…".to_owned()) && all.contains(&"About tinyclient".to_owned()));
    }
}
