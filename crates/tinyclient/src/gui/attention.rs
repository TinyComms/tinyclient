//! The window's part in notifications, which chatd posts (`chatd/src/notify.rs`): it tells
//! chatd when it has focus, so the conversation being read never notifies; it comes forward on
//! the conversation a clicked notification names (`Change::Open`, or `--open` when chatd had to
//! start it); and it keeps the taskbar button or Dock icon's number (N11) and flashes it for
//! something new while it is in the background (N10).

use super::Shell;
use gpui::Context;
use protocol::settings::{BADGE, FLASH};
use protocol::{AccountStatus, Key};
use std::cell::{Cell, RefCell};

thread_local! {
    /// The total last shown, to flash only when it grows.
    static SHOWN: Cell<Option<u32>> = const { Cell::new(None) };
    /// `--open` at launch, held until the first status, when there is a list to open it in.
    static PENDING: RefCell<Option<Key>> = const { RefCell::new(None) };
}

/// `--open=<key as JSON>`: chatd started the window for a clicked notification.
pub fn read_launch() {
    let asked = std::env::args().find_map(|arg| arg.strip_prefix("--open=").map(str::to_owned));
    if let Some(key) = asked.and_then(|json| serde_json::from_str::<Key>(&json).ok()) {
        PENDING.with(|pending| *pending.borrow_mut() = Some(key));
    }
}

/// What the taskbar shows: the sum of every account's number, as the rail draws it.
pub fn total(accounts: &[AccountStatus]) -> u32 {
    accounts.iter().map(|account| account.attention).sum()
}

/// Whether a new total should flash the button: it grew, the window is in the background, and
/// N10 is on. The first total after launch never flashes.
pub fn flashes(before: Option<u32>, now: u32, active: bool, flash: bool) -> bool {
    flash && !active && before.is_some_and(|before| now > before)
}

impl Shell {
    /// The window gained or lost focus.
    pub(super) fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.people_state.hub().watch_focus(focused, cx);
    }

    /// A notification was clicked: forward, on its conversation.
    pub(super) fn open_from_notification(&mut self, conversation: &Key, cx: &mut Context<Self>) {
        #[cfg(any(windows, target_os = "macos"))]
        notifier::taskbar::raise();
        cx.activate(true);
        self.select(conversation.clone(), "", cx);
        cx.notify();
    }

    /// The accounts' numbers changed (`apply_status`).
    pub(super) fn attention_changed(&mut self, accounts: &[AccountStatus], cx: &mut Context<Self>) {
        // The first status is also when chatd first hears whether the window has focus.
        self.focus_changed(self.active, cx);
        if let Some(key) = PENDING.with(|pending| pending.borrow_mut().take()) {
            self.open_from_notification(&key, cx);
        }
        let values = super::app::prefs::values();
        let now = total(accounts);
        let before = SHOWN.replace(Some(now));
        if before == Some(now) {
            return;
        }
        show(if values.on(BADGE) { now } else { 0 }, flashes(before, now, self.active, values.on(FLASH)));
    }

    /// N11 was turned on or off: show or clear the number now.
    pub(super) fn badge_setting_changed(&mut self) {
        let shown = SHOWN.get().unwrap_or(0);
        show(if super::app::prefs::values().on(BADGE) { shown } else { 0 }, false);
    }
}

/// The number on the button or icon, and a flash. Linux has neither here: a desktop's own
/// launcher badge (Unity's LauncherEntry) is not spoken yet.
fn show(badge: u32, flash: bool) {
    #[cfg(any(windows, target_os = "macos"))]
    {
        notifier::taskbar::badge(badge);
        if flash {
            notifier::taskbar::flash();
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (badge, flash);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_flashes_only_for_more_while_in_the_background() {
        assert!(flashes(Some(2), 3, false, true));
        assert!(!flashes(Some(3), 2, false, true), "fewer is reading, not news");
        assert!(!flashes(Some(2), 3, true, true), "the window is in front");
        assert!(!flashes(Some(2), 3, false, false), "N10 is off");
        assert!(!flashes(None, 3, false, true), "what was there at launch");
    }
}
