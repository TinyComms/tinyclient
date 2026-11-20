//! The window's part in notifications, which chatd posts (`chatd/src/notify.rs`): it tells
//! chatd when it has focus, so the conversation being read never notifies; it comes forward on
//! the conversation a clicked notification names (`Change::Open`, or `--open` when chatd had to
//! start it); and it keeps the taskbar button, Dock icon or launcher entry's number (N11) and
//! flashes or bounces it for something new while it is in the background (N10).

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
    /// The window whose manager a flash asks. The other systems find their own (`taskbar`).
    #[cfg(target_os = "linux")]
    static WINDOW: Cell<Option<gpui::AnyWindowHandle>> = const { Cell::new(None) };
}

/// The window, noted when it opens, so a flash can reach it without a redraw in hand.
#[cfg(target_os = "linux")]
pub(super) fn note_window(window: gpui::AnyWindowHandle) {
    WINDOW.with(|slot| slot.set(Some(window)));
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
        let flash = flashes(before, now, self.active, values.on(FLASH));
        show(if values.on(BADGE) { now } else { 0 }, flash);
        // The flash asks the window, which only this update's context can reach; the number
        // went to the launcher entry without it (`notifier::taskbar`).
        #[cfg(target_os = "linux")]
        if flash {
            demand_attention(cx);
        }
    }

    /// N11 was turned on or off: show or clear the number now.
    pub(super) fn badge_setting_changed(&mut self) {
        let shown = SHOWN.get().unwrap_or(0);
        show(if super::app::prefs::values().on(BADGE) { shown } else { 0 }, false);
    }
}

/// The number on the button, icon or launcher entry, and a flash. On Linux the number goes to
/// the desktop's launcher entry over the session bus, and the flash is asked of the window
/// manager in `attention_changed`, whose context can reach the window.
fn show(badge: u32, flash: bool) {
    #[cfg(any(windows, target_os = "macos"))]
    {
        notifier::taskbar::badge(badge);
        if flash {
            notifier::taskbar::flash();
        }
    }
    #[cfg(target_os = "linux")]
    {
        notifier::taskbar::badge(badge);
        let _ = flash;
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let _ = (badge, flash);
}

/// The window manager's attention for something new: X11's urgency hint
/// (`_NET_WM_STATE_DEMANDS_ATTENTION` through GPUI's `Window::request_attention`), which the
/// manager clears when the window comes to front. Wayland has no such hint, where the call is
/// nothing and the entry's number is the only sign.
#[cfg(target_os = "linux")]
fn demand_attention(cx: &mut Context<Shell>) {
    WINDOW.with(|window| {
        if let Some(window) = window.get() {
            let _ = window.update(cx, |_, window, _| window.request_attention());
        }
    });
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
