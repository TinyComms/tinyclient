//! The tray icon of the window process (Windows; G3 in `docs/product/09-settings.md`): the way
//! back into a window that closing hid, and a Quit that really quits. Like [`taskbar`], it
//! belongs to the window process and is called on its main thread — GPUI's loop already pumps
//! the messages the icon's hidden window receives, and [`Shell_NotifyIconW`] only posts to it.
//! Linux keeps chatd up through its systemd unit until its StatusNotifierItem lands; macOS
//! has Keep receiving (G2) instead of an icon, so both stay out of here for now.
//!
//! The icon exists exactly while the setting is on ([`set`]), and when the shell remakes the
//! taskbar under it (an explorer restart) it comes back on the `TaskbarCreated` broadcast.
//! Closing the window with it up hides the window ([`close_into_tray`]) and leaves chatd
//! running — the process never left,
//! so nothing else has to hold the icon. The icon's click opens the window again; its menu's
//! Quit sets a flag ([`take_quit`]) that the window's own loop reads, so the ask crosses no
//! thread but a flag and runs the menu bar's quit, which stops chatd as every way out does.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, EnumWindows, GetCursorPos,
    GetWindow, GetWindowTextW, GetWindowThreadProcessId, HICON, IsIconic, LoadIconW, MF_STRING, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, ShowWindow, ShowWindowAsync, TrackPopupMenu, WNDCLASSW, GW_OWNER, SW_HIDE,
    SW_RESTORE, SW_SHOW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP,
};

/// The one icon this process adds. One per process is all the shell shows well.
const ID: u32 = 1;
/// The message the shell posts to the hidden window for everything the icon does.
const CALLBACK: u32 = WM_APP + 1;
const CLASS: PCWSTR = w!("tinyclient_tray");

/// The menu's items in the order they draw: our id for it, and the words. Nothing is offered
/// that does nothing, as the menu bar's rule goes; Pause notifications and the unread line
/// come with what they do.
const ITEMS: [(u32, &str); 2] = [(1, "Open tinyclient"), (2, "Quit tinyclient")];

/// What a click on the icon means. At callback version 0 one click arrives as exactly one Up
/// message, so the Up events alone carry it and never double.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Click {
    Open,
    Menu,
}

fn click(msg: u32) -> Option<Click> {
    match msg {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK => Some(Click::Open),
        WM_RBUTTONUP => Some(Click::Menu),
        _ => None,
    }
}

/// What a chosen menu id asks for. Open is done here; Quit waits as a flag for GPUI's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Open,
    Quit,
}

fn ask(id: u32) -> Option<Ask> {
    match id {
        1 => Some(Ask::Open),
        2 => Some(Ask::Quit),
        _ => None,
    }
}

/// The balloon's words, shown once per run when the first close hides the window.
fn still_running() -> (&'static str, &'static str) {
    ("Still running", "tinyclient keeps receiving here. This icon is how it comes back.")
}

/// The hidden window, 0 while there is no icon.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The menu's Quit, set in the window procedure and read by GPUI's own loop.
static QUIT: AtomicBool = AtomicBool::new(false);
/// The balloon showed once per run; after that, closing says nothing.
static NOTED: AtomicBool = AtomicBool::new(false);
/// The number of the shell's `TaskbarCreated` broadcast, 0 until asked for: the taskbar was
/// rebuilt and the icon with it, so this icon has to go back up.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Whether the icon is up now. Close asks this: the promise "the icon is the way back in"
/// only holds while it exists, so without it closing keeps quitting as it always did.
pub fn active() -> bool {
    WINDOW.load(Ordering::SeqCst) != 0
}

/// Creates or removes the icon, on the main thread. The window's start calls this with the
/// choice read from prefs, and Settings calls it again with every change of the switch.
pub fn set(on: bool) {
    if on {
        add();
    } else {
        remove();
    }
}

/// Asks the shell once for the number of its `TaskbarCreated` broadcast — what it sends every
/// top-level window when the taskbar is rebuilt (explorer restarted) and every tray icon with
/// it. The same name answers the same number for the length of the session.
fn register_taskbar_created() {
    if TASKBAR_CREATED.load(Ordering::Relaxed) != 0 {
        return;
    }
    // SAFETY: a plain lookup of the shell's fixed message name.
    TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, Ordering::Relaxed);
}

/// Whether the menu's Quit has been asked since the last read, for the window's own loop.
pub fn take_quit() -> bool {
    QUIT.swap(false, Ordering::SeqCst)
}

/// Closing the window with the setting on: hide it, and say once where it went. chatd keeps
/// running — the process never left, and the icon stays the way back in.
pub fn close_into_tray() {
    let Some(window) = main_window() else { return };
    // SAFETY: our own window; the same call gpui's minimize path makes.
    unsafe {
        let _ = ShowWindowAsync(window, SW_HIDE);
    }
    let tray = HWND(WINDOW.load(Ordering::SeqCst) as *mut core::ffi::c_void);
    if tray.is_invalid() || NOTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (title, words) = still_running();
    let mut text = [0u16; 200];
    for (at, c) in words.encode_utf16().take(text.len() - 1).enumerate() {
        text[at] = c;
    }
    let mut head = [0u16; 64];
    for (at, c) in title.encode_utf16().take(head.len() - 1).enumerate() {
        head[at] = c;
    }
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: tray,
        uID: ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_NOSOUND,
        szInfo: text,
        szInfoTitle: head,
        ..Default::default()
    };
    // SAFETY: a filled-in modify of our own icon.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn add() {
    if active() {
        return;
    }
    register_taskbar_created();
    let Some(window) = hidden_window() else {
        tracing::warn!("tinyclient: tray window refused");
        return;
    };
    let mut tip = [0u16; 128];
    for (at, c) in "tinyclient".encode_utf16().enumerate() {
        tip[at] = c;
    }
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_SHOWTIP,
        uCallbackMessage: CALLBACK,
        hIcon: icon().unwrap_or_default(),
        szTip: tip,
        ..Default::default()
    };
    // SAFETY: a filled-in struct naming our window; the shell keeps the shared icon until
    // NIM_DELETE, and a shared icon is never destroyed.
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        tracing::warn!("tinyclient: tray icon refused");
        // SAFETY: the window made here is ours and has done nothing yet.
        unsafe {
            let _ = DestroyWindow(window);
        }
        return;
    }
    WINDOW.store(window.0 as isize, Ordering::SeqCst);
}

/// Takes the icon and its window down. The clean quit paths call this, so no ghost icon is
/// left for a hover to prune; a hard death leaves the picture, as every tray app's does.
pub fn remove() {
    let window = HWND(WINDOW.swap(0, Ordering::SeqCst) as *mut core::ffi::c_void);
    if window.is_invalid() {
        return;
    }
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ID,
        ..Default::default()
    };
    // SAFETY: removing our own icon before the window that carries it dies.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        let _ = DestroyWindow(window);
    }
}

/// The hidden window the shell talks to. Created on the caller's thread — GPUI's main
/// thread — and never shown, so it is neither titled nor visible and the hung-window watch
/// (`gui::os_window_alive`) reads only the real window.
fn hidden_window() -> Option<HWND> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if !REGISTERED.swap(true, Ordering::SeqCst) {
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), lpszClassName: CLASS, ..Default::default() };
        // SAFETY: a static class name and proc, registered once per process.
        unsafe { RegisterClassW(&wc) };
    }
    // SAFETY: no parent, no menu, never shown; the process's own message loop pumps it.
    unsafe {
        CreateWindowExW(Default::default(), CLASS, w!(""), Default::default(), 0, 0, 0, 0, None, None, None, None).ok()
    }
}

unsafe extern "system" fn wnd_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == CALLBACK {
        match click(lparam.0 as u32) {
            Some(Click::Open) => open(),
            Some(Click::Menu) => menu(window),
            None => {}
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        WINDOW.store(0, Ordering::SeqCst);
        return LRESULT(0);
    }
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::SeqCst) {
        // The shell rebuilt the taskbar and the icon with it, so ours is gone while
        // `WINDOW` still names a live window: the same rebuild [`set`] does puts it back.
        remove();
        add();
        return LRESULT(0);
    }
    // SAFETY: the default answer for a message this window has no other opinion on.
    unsafe { DefWindowProcW(window, msg, wparam, lparam) }
}

/// The icon's click: the window, restored and in front. The same calls the taskbar's raise
/// makes; the click itself is the person asking, so no foreground permission is needed first.
fn open() {
    let Some(window) = main_window() else { return };
    // SAFETY: plain calls on our own window.
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        } else {
            let _ = ShowWindow(window, SW_SHOW);
        }
        let _ = SetForegroundWindow(window);
    }
}

/// The icon's right click: the menu where the pointer is. `TPM_RETURNCMD` hands the chosen id
/// straight back; the foreground dance before it is the shell's rule for a menu that closes
/// on a click outside itself.
fn menu(window: HWND) {
    let Ok(items) = unsafe { CreatePopupMenu() } else { return };
    for (id, words) in ITEMS {
        let wide: Vec<u16> = words.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a fresh menu; the string outlives the call.
        unsafe {
            let _ = AppendMenuW(items, MF_STRING, id as usize, PCWSTR(wide.as_ptr()));
        }
    }
    let mut at = POINT::default();
    // SAFETY: a plain call; the pointer is where a person expects the menu.
    unsafe {
        let _ = GetCursorPos(&mut at);
        let _ = SetForegroundWindow(window);
        let picked = TrackPopupMenu(items, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, at.x, at.y, Some(0), window, None);
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        match ask(picked.0 as u32) {
            Some(Ask::Open) => open(),
            Some(Ask::Quit) => QUIT.store(true, Ordering::SeqCst),
            None => {}
        }
    }
    // SAFETY: the menu is made here and shown above; nothing else holds it.
    unsafe {
        let _ = DestroyMenu(items);
    }
}

/// The app's own icon, resource 1 (`build.rs` embeds `packaging/windows/tinyclient.ico`),
/// the same one Explorer and the taskbar show. A shared icon: the shell keeps it.
fn icon() -> Option<HICON> {
    // SAFETY: reads our own module's resource table.
    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    let icon = unsafe { LoadIconW(Some(HINSTANCE(module.0)), PCWSTR(1 as _)) }.ok()?;
    Some(icon)
}

/// This process's main window: top level, owned by nobody, with the app's title. A hidden
/// window keeps its title, so this finds it again after [`close_into_tray`].
fn main_window() -> Option<HWND> {
    unsafe extern "system" fn each(window: HWND, found: LPARAM) -> BOOL {
        let mut pid = 0u32;
        // SAFETY: reading the window's owner process and title; `found` points at the
        // `Option<HWND>` below for the length of EnumWindows.
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid != std::process::id() || GetWindow(window, GW_OWNER).is_ok() {
                return BOOL(1);
            }
            let mut title = [0u16; 16];
            if GetWindowTextW(window, &mut title) > 0 {
                *(found.0 as *mut Option<HWND>) = Some(window);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut found: Option<HWND> = None;
    // SAFETY: the callback only writes `found`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut found as *mut Option<HWND> as isize)) };
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_says_open_and_quit_and_the_ids_decide() {
        assert_eq!(ITEMS, [(1, "Open tinyclient"), (2, "Quit tinyclient")]);
        for (id, _) in ITEMS {
            assert!(ask(id).is_some(), "{id} maps to nothing");
        }
        assert_eq!(ask(0), None);
        assert_eq!(ask(3), None);
        assert_eq!(ask(2), Some(Ask::Quit), "the last item is the quit");
    }

    #[test]
    fn one_click_is_one_meaning() {
        assert_eq!(click(WM_LBUTTONUP), Some(Click::Open));
        assert_eq!(click(WM_LBUTTONDBLCLK), Some(Click::Open));
        assert_eq!(click(WM_RBUTTONUP), Some(Click::Menu));
        // The Down of the same click, and everything else, means nothing.
        assert_eq!(click(0x201), None);
        assert_eq!(click(WM_NULL), None);
        assert_eq!(click(CALLBACK), None);
    }

    #[test]
    fn the_balloon_fits_what_the_icon_can_show() {
        let (title, words) = still_running();
        assert!(title.encode_utf16().count() < 64, "szInfoTitle holds 64 with its nul");
        assert!(words.encode_utf16().count() < 200, "szInfo holds 200 with its nul");
        assert!(!title.is_empty() && !words.is_empty());
    }
}
