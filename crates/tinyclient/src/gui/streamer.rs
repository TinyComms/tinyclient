//! Streamer mode runtime state and OS window capture-exclusion hooks (`#327`).

use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static EXCLUDED: AtomicBool = AtomicBool::new(false);

/// Whether Streamer mode is currently active in the shell.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Updates the shell's active Streamer mode flag and returns whether it changed.
pub fn set_active(on: bool) -> bool {
    ACTIVE.swap(on, Ordering::Relaxed) != on
}

/// Pure decision helper matching `protocol::settings::Values::streamer_active`.
pub fn active(mode: &str, sharing: bool, detect_obs: bool, obs_running: bool) -> bool {
    match mode {
        "on" => true,
        "while_sharing" => sharing || (detect_obs && obs_running),
        _ => false,
    }
}

/// Whether `text` contains an invite link or sign-in code that Streamer mode hides until clicked.
pub fn contains_sensitive(text: &str) -> bool {
    protocol::streamer::contains_sensitive(text)
}

/// Applies or clears OS-level capture exclusion on the application's windows.
///
/// While exclusion is on, every call reapplies it so a window opened afterwards is covered.
/// Turning it off applies once.
///
/// - Windows: `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (`0x11`) / `WDA_NONE` (`0`).
/// - macOS: `NSWindow.setSharingType:` (`NSWindowSharingNone = 0` / `NSWindowSharingReadOnly = 1`).
/// - Other platforms: no-op.
pub fn apply_capture_exclusion(exclude: bool) {
    if !exclude && EXCLUDED.swap(false, Ordering::Relaxed) == false {
        return;
    }
    EXCLUDED.store(exclude, Ordering::Relaxed);
    set_window_capture_exclusion(exclude);
}

/// Whether this joined call is sending a screen share. Same rule chatd uses to silence notifications.
pub fn sharing(call: Option<&protocol::Call>) -> bool {
    call.is_some_and(|call| {
        call.state == protocol::CallState::Joined
            && call.video.iter().any(|stream| {
                stream.direction == protocol::Direction::Encode && stream.content == protocol::Content::Share
            })
    })
}

/// Reads the settings, updates [`is_active`], and applies capture exclusion.
/// `true` when the active flag changed.
pub fn sync(sharing: bool) -> bool {
    let values = crate::gui::app::prefs::values();
    let mode = values.word(protocol::settings::GLOBAL, protocol::settings::STREAMER_MODE);
    let detect = values.on(protocol::settings::STREAMER_DETECT_OBS);
    let obs = if detect && mode == "while_sharing" {
        protocol::streamer::obs_running()
    } else {
        false
    };
    let on = values.streamer_active(sharing, obs);
    let changed = set_active(on);
    apply_capture_exclusion(on && values.on(protocol::settings::STREAMER_HIDE_WINDOW));
    WANT_WATCH.store(detect && mode == "while_sharing", Ordering::Relaxed);
    changed
}

static WANT_WATCH: AtomicBool = AtomicBool::new(false);
static WATCH_ARMED: AtomicBool = AtomicBool::new(false);

/// OBS detection only matters for the default "while sharing" mode, and only when its switch is on.
/// [`sync`] records this so a later poll does not read the settings again.
pub fn watch_wanted() -> bool {
    WANT_WATCH.load(Ordering::Relaxed)
}

/// Claims the one OBS poll. `false` when detection is off or a poll is already running.
pub fn take_watch() -> bool {
    watch_wanted() && !WATCH_ARMED.swap(true, Ordering::Relaxed)
}

pub fn release_watch() {
    WATCH_ARMED.store(false, Ordering::Relaxed);
}

#[cfg(windows)]
fn set_window_capture_exclusion(exclude: bool) {
    const WDA_NONE: u32 = 0x0000_0000;
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumThreadWindows(
            dw_thread_id: u32,
            lpfn: unsafe extern "system" fn(isize, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn SetWindowDisplayAffinity(hwnd: isize, dw_affinity: u32) -> i32;
    }

    unsafe extern "system" fn apply_to_hwnd(hwnd: isize, lparam: isize) -> i32 {
        let affinity = lparam as u32;
        let _ = SetWindowDisplayAffinity(hwnd, affinity);
        1
    }

    let affinity = if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    unsafe {
        let tid = GetCurrentThreadId();
        let _ = EnumThreadWindows(tid, apply_to_hwnd, affinity as isize);
    }
}

#[cfg(target_os = "macos")]
fn set_window_capture_exclusion(exclude: bool) {
    use std::ffi::{c_char, c_void};

    type Id = *mut c_void;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }

    // NSWindowSharingNone = 0, NSWindowSharingReadOnly = 1 (macOS default).
    let sharing_type: usize = if exclude { 0 } else { 1 };
    unsafe {
        let send_id: extern "C" fn(Id, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_usize: extern "C" fn(Id, Id) -> usize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let at: extern "C" fn(Id, Id, usize) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let set_sharing: extern "C" fn(Id, Id, usize) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let ns_app_cls = objc_getClass(c"NSApplication".as_ptr());
        if ns_app_cls.is_null() {
            return;
        }
        let app = send_id(ns_app_cls, sel_registerName(c"sharedApplication".as_ptr()));
        if app.is_null() {
            return;
        }
        let windows = send_id(app, sel_registerName(c"windows".as_ptr()));
        if windows.is_null() {
            return;
        }
        let count = send_usize(windows, sel_registerName(c"count".as_ptr()));
        let obj_at = sel_registerName(c"objectAtIndex:".as_ptr());
        let set_sel = sel_registerName(c"setSharingType:".as_ptr());
        for ix in 0..count {
            let win = at(windows, obj_at, ix);
            if !win.is_null() {
                set_sharing(win, set_sel, sharing_type);
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn set_window_capture_exclusion(_exclude: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamer_active_decision_covers_all_three_modes_and_obs() {
        assert!(!active("off", true, true, true));
        assert!(active("on", false, false, false));
        assert!(!active("while_sharing", false, false, false));
        assert!(active("while_sharing", true, false, false));
        assert!(!active("while_sharing", false, false, true));
        assert!(active("while_sharing", false, true, true));
    }

    #[test]
    fn contains_sensitive_matches_invites_and_otp_codes() {
        assert!(contains_sensitive("Join https://discord.gg/secret123"));
        assert!(contains_sensitive("Your Tiny sign-in code is 918273"));
        assert!(!contains_sensitive("Just a normal chat message"));
    }
}
