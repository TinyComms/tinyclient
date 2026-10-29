//! The voice keys that work in any app (Settings › Voice, V13 and V15 to V17): push-to-talk,
//! push-to-talk with priority, toggle mute, toggle deafen. Each is a setting chatd keeps
//! (`protocol::settings::PTT_KEY` and the rest) as a [`Binding`]; this file watches for them.
//!
//! A thread asks the system a hundred times a second whether each bound key is down, which needs
//! no hook into other apps' input and no administrator: `GetAsyncKeyState` on Windows (keys and
//! the middle and side mouse buttons), `CGEventSourceKeyState` on macOS (with Input Monitoring
//! allowed; without it every key reads up and Settings says what to allow), `XQueryKeymap` on
//! X11 through a libX11 opened at run time. Wayland lets no app see keys meant for another, so
//! there mute and deafen keys work while tinyclient's window is in front, and push-to-talk does
//! not; Settings says so.
//!
//! The talk keys go to chatd from the watching thread itself, so their downs and ups arrive in
//! the order they happened (`CallTalk`; with no call chatd refuses and nothing happens). Mute
//! and deafen go to the shell, which knows the call and does what ⌘⇧M and ⌘⇧D do.

use super::app::prefs;
use super::state::Shell;
use gpui::{App, Context, Keystroke, MouseButton, NavigationDirection, WeakEntity};
use protocol::hotkey::{Binding, Key, Modifiers};
use protocol::settings::{DEAFEN_KEY, MUTE_KEY, PRIORITY_KEY, PTT_KEY};
use protocol::Request;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

/// How often the keys are looked at: 10 ms, well inside a word's first syllable.
const POLL: Duration = Duration::from_millis(10);
/// Polls between reads of the bindings, which change only in Settings.
const REREAD: u32 = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Talk,
    Priority,
    Mute,
    Deafen,
}

impl Action {
    pub const ALL: [Action; 4] = [Action::Talk, Action::Priority, Action::Mute, Action::Deafen];

    /// The setting it is kept under.
    pub fn key(self) -> &'static str {
        match self {
            Action::Talk => PTT_KEY,
            Action::Priority => PRIORITY_KEY,
            Action::Mute => MUTE_KEY,
            Action::Deafen => DEAFEN_KEY,
        }
    }

    /// As Settings names it, in a clash: "F13 is already Toggle mute."
    pub fn name(self) -> &'static str {
        match self {
            Action::Talk => "Push to talk",
            Action::Priority => "Priority push to talk",
            Action::Mute => "Toggle mute",
            Action::Deafen => "Toggle deafen",
        }
    }
}

/// How far the keys reach on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// In any app.
    Everywhere,
    /// In any app once the system allows it (macOS Input Monitoring).
    Permission,
    /// Only while tinyclient's window is in front, and push-to-talk not at all.
    Window,
}

/// What this system lets the keys do now.
pub fn reach() -> Reach {
    platform::reach()
}

/// Asks the system to let the keys be watched (macOS: the Input Monitoring prompt, once).
pub fn ask_permission() {
    platform::ask_permission();
}

/// Whether the mouse's side buttons can be watched here.
pub fn side_buttons() -> bool {
    platform::SIDE_BUTTONS
}

/// Set while Settings records a key, so pressing it there does not also talk or mute.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// The push-to-talk keys held right now: talk, and talk with priority. Set by the watching
/// thread, read by the call views to light their push-to-talk pill.
static TALK_HELD: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Whether a push-to-talk key is held, and whether it is the priority one.
pub fn talk_held() -> Option<bool> {
    let [talk, priority] = &TALK_HELD;
    if priority.load(Ordering::Relaxed) {
        Some(true)
    } else {
        talk.load(Ordering::Relaxed).then_some(false)
    }
}

pub fn pause(on: bool) {
    PAUSED.store(on, Ordering::Relaxed);
}

/// Starts watching for the keys, for as long as the app runs. Called once, by the shell.
pub fn start(shell: WeakEntity<Shell>, cx: &mut App) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let (toggles, toggled) = channel::<Action>();
    if reach() == Reach::Window {
        in_window(toggles, cx);
    } else {
        let spawned = std::thread::Builder::new().name("hotkeys".to_owned()).stack_size(256 * 1024).spawn(move || watch(&toggles));
        if let Err(err) = spawned {
            eprintln!("tinyclient: the voice keys could not be watched: {err}");
            return;
        }
    }
    cx.spawn(async move |cx| {
        let mut toggled = toggled;
        loop {
            let (back, action) = cx.background_executor().spawn(async move { let action = toggled.recv(); (toggled, action) }).await;
            toggled = back;
            let Ok(action) = action else {
                return;
            };
            let _ = shell.update(cx, |shell, cx| shell.hotkey(action, cx));
        }
    })
    .detach();
}

/// Every binding, as Settings last stored them.
fn bindings() -> [Option<Binding>; 4] {
    let values = prefs::values();
    Action::ALL.map(|action| values.hotkey(action.key()))
}

/// The watching thread. Never returns while the app runs.
fn watch(toggles: &Sender<Action>) {
    let Some(mut keys) = platform::Keys::open() else {
        eprintln!("tinyclient: the keyboard cannot be watched here; voice keys work in the window only");
        return;
    };
    let mut bound = bindings();
    let mut held = [false; 4];
    let mut polls = 0u32;
    loop {
        std::thread::sleep(POLL);
        polls = polls.wrapping_add(1);
        if polls.is_multiple_of(REREAD) {
            bound = bindings();
        }
        let paused = PAUSED.load(Ordering::Relaxed);
        if !paused && bound.iter().any(Option::is_some) {
            keys.poll();
        }
        for (at, action) in Action::ALL.into_iter().enumerate() {
            let down = !paused && bound[at].is_some_and(|binding| pressed(&mut keys, &binding));
            if down == held[at] {
                continue;
            }
            held[at] = down;
            match action {
                Action::Talk | Action::Priority => {
                    let priority = action == Action::Priority;
                    let talk = Request::CallTalk { held: down, priority };
                    // No call: chatd says so, and there is nothing to do about it.
                    let _ = crate::client::ask(&talk);
                    TALK_HELD[usize::from(priority)].store(down, Ordering::Relaxed);
                    // The shell redraws its push-to-talk pill.
                    if toggles.send(action).is_err() {
                        return;
                    }
                }
                // The shell is gone: so is the app.
                Action::Mute | Action::Deafen if down && toggles.send(action).is_err() => return,
                _ => {}
            }
        }
    }
}

/// Whether the binding's key is down with at least its modifiers. Other modifiers held as well
/// do not matter, so push-to-talk keeps working while typing a capital.
fn pressed(keys: &mut platform::Keys, binding: &Binding) -> bool {
    let m = binding.modifiers;
    (!m.ctrl || keys.down(Key::Ctrl)) && (!m.alt || keys.down(Key::Alt)) && (!m.shift || keys.down(Key::Shift)) && (!m.meta || keys.down(Key::Meta)) && keys.down(binding.key)
}

/// Where no app may watch the keyboard (Wayland): the mute and deafen keys, while a tinyclient
/// window has the keys.
fn in_window(toggles: Sender<Action>, cx: &mut App) {
    cx.observe_keystrokes(move |event, _, _| {
        if PAUSED.load(Ordering::Relaxed) {
            return;
        }
        let Some(pressed) = binding_of(&event.keystroke) else {
            return;
        };
        let bound = bindings();
        for (at, action) in [(2, Action::Mute), (3, Action::Deafen)] {
            if bound[at] == Some(pressed) {
                let _ = toggles.send(action);
            }
        }
    })
    .detach();
}

/// The binding a key press in a tinyclient window makes. Escape is not one: it cancels.
pub fn binding_of(keystroke: &Keystroke) -> Option<Binding> {
    let key = Key::parse(keystroke.key.as_str())?;
    let m = keystroke.modifiers;
    Some(Binding { modifiers: Modifiers { ctrl: m.control, alt: m.alt, shift: m.shift, meta: m.platform }, key })
}

/// The binding a mouse button makes: the middle one and the two side ones only.
pub fn mouse_binding(button: MouseButton) -> Option<Binding> {
    let key = match button {
        MouseButton::Middle => Key::Mouse3,
        MouseButton::Navigate(NavigationDirection::Back) => Key::Mouse4,
        MouseButton::Navigate(NavigationDirection::Forward) => Key::Mouse5,
        _ => return None,
    };
    Some(Binding::new(key))
}

/// A modifier pressed and let go with nothing else: the modifier alone is the key.
pub fn modifier_binding(was: gpui::Modifiers) -> Option<Binding> {
    let key = if was.control {
        Key::Ctrl
    } else if was.alt {
        Key::Alt
    } else if was.shift {
        Key::Shift
    } else if was.platform {
        Key::Meta
    } else {
        return None;
    };
    Some(Binding::new(key))
}

impl Shell {
    /// A global mute or deafen key went down.
    fn hotkey(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Mute => self.call_mute(cx),
            Action::Deafen => self.call_deafen(cx),
            // Held or let go: the push-to-talk pill lights or goes out.
            Action::Talk | Action::Priority => cx.notify(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{Key, Reach};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    pub const SIDE_BUTTONS: bool = true;

    pub fn reach() -> Reach {
        Reach::Everywhere
    }

    pub fn ask_permission() {}

    /// The virtual-key code of a key on a US layout; the system maps it to the key in the
    /// same place on any other.
    fn code(key: Key) -> Option<i32> {
        Some(match key {
            Key::Letter(c) => c.to_ascii_uppercase() as i32,
            Key::Digit(c) => c as i32,
            Key::F(n) => 0x6f + i32::from(n),
            Key::Space => 0x20,
            Key::Tab => 0x09,
            Key::Enter => 0x0d,
            Key::Backspace => 0x08,
            Key::Insert => 0x2d,
            Key::Delete => 0x2e,
            Key::Home => 0x24,
            Key::End => 0x23,
            Key::PageUp => 0x21,
            Key::PageDown => 0x22,
            Key::Left => 0x25,
            Key::Up => 0x26,
            Key::Right => 0x27,
            Key::Down => 0x28,
            Key::CapsLock => 0x14,
            Key::Punctuation(c) => match c {
                ';' => 0xba,
                '=' => 0xbb,
                ',' => 0xbc,
                '-' => 0xbd,
                '.' => 0xbe,
                '/' => 0xbf,
                '`' => 0xc0,
                '[' => 0xdb,
                '\\' => 0xdc,
                ']' => 0xdd,
                '\'' => 0xde,
                _ => return None,
            },
            Key::Ctrl => 0x11,
            Key::Alt => 0x12,
            Key::Shift => 0x10,
            // Either Windows key.
            Key::Meta => 0x5b,
            Key::Mouse3 => 0x04,
            Key::Mouse4 => 0x05,
            Key::Mouse5 => 0x06,
        })
    }

    pub struct Keys;

    impl Keys {
        pub fn open() -> Option<Keys> {
            Some(Keys)
        }

        /// Each key is asked about as it is wanted: nothing to read ahead.
        pub fn poll(&mut self) {}

        pub fn down(&mut self, key: Key) -> bool {
            let held = |code: i32| {
                // SAFETY: a plain query of the key's state; any code is accepted.
                let state = unsafe { GetAsyncKeyState(code) };
                state as u16 & 0x8000 != 0
            };
            match (key, code(key)) {
                (Key::Meta, _) => held(0x5b) || held(0x5c),
                (_, Some(code)) => held(code),
                (_, None) => false,
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Key, Reach};

    pub const SIDE_BUTTONS: bool = true;

    /// `kCGEventSourceStateHIDSystemState`: the keys as the hardware has them, whichever app is
    /// in front.
    const HID: i32 = 1;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
        fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }

    pub fn reach() -> Reach {
        // SAFETY: asks whether this process may listen to input; no arguments.
        if unsafe { CGPreflightListenEventAccess() } {
            Reach::Everywhere
        } else {
            Reach::Permission
        }
    }

    pub fn ask_permission() {
        // SAFETY: shows the system's Input Monitoring prompt once; no arguments.
        unsafe {
            CGRequestListenEventAccess();
        }
    }

    /// `kVK_*` from Carbon's Events.h: where a key is on a US layout.
    fn code(key: Key) -> Option<u16> {
        const LETTERS: [u16; 26] = [0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06];
        const DIGITS: [u16; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
        const F: [u16; 20] = [0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71, 0x6a, 0x40, 0x4f, 0x50, 0x5a];
        Some(match key {
            Key::Letter(c) => LETTERS[(c as u8 - b'a') as usize],
            Key::Digit(c) => DIGITS[(c as u8 - b'0') as usize],
            Key::F(n) => *F.get(usize::from(n) - 1)?,
            Key::Space => 0x31,
            Key::Tab => 0x30,
            Key::Enter => 0x24,
            Key::Backspace => 0x33,
            Key::Insert => 0x72,
            Key::Delete => 0x75,
            Key::Home => 0x73,
            Key::End => 0x77,
            Key::PageUp => 0x74,
            Key::PageDown => 0x79,
            Key::Left => 0x7b,
            Key::Right => 0x7c,
            Key::Down => 0x7d,
            Key::Up => 0x7e,
            Key::CapsLock => 0x39,
            Key::Punctuation(c) => match c {
                '=' => 0x18,
                '-' => 0x1b,
                ']' => 0x1e,
                '[' => 0x21,
                '\'' => 0x27,
                ';' => 0x29,
                '\\' => 0x2a,
                ',' => 0x2b,
                '/' => 0x2c,
                '.' => 0x2f,
                '`' => 0x32,
                _ => return None,
            },
            Key::Ctrl | Key::Alt | Key::Shift | Key::Meta | Key::Mouse3 | Key::Mouse4 | Key::Mouse5 => return None,
        })
    }

    pub struct Keys;

    impl Keys {
        pub fn open() -> Option<Keys> {
            Some(Keys)
        }

        /// Each key is asked about as it is wanted: nothing to read ahead.
        pub fn poll(&mut self) {}

        pub fn down(&mut self, key: Key) -> bool {
            // SAFETY: plain queries of key and button state.
            let held = |code: u16| unsafe { CGEventSourceKeyState(HID, code) };
            let button = |n: u32| unsafe { CGEventSourceButtonState(HID, n) };
            match key {
                // Each modifier, left or right.
                Key::Ctrl => held(0x3b) || held(0x3e),
                Key::Alt => held(0x3a) || held(0x3d),
                Key::Shift => held(0x38) || held(0x3c),
                Key::Meta => held(0x37) || held(0x36),
                Key::Mouse3 => button(2),
                Key::Mouse4 => button(3),
                Key::Mouse5 => button(4),
                _ => code(key).is_some_and(held),
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{Key, Reach};
    use std::ffi::{c_char, c_int, c_uchar, c_uint, c_ulong, c_void};

    /// X11 reports only the first five pointer buttons in its state: the side ones are not there.
    pub const SIDE_BUTTONS: bool = false;

    pub fn reach() -> Reach {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland");
        if wayland || std::env::var_os("DISPLAY").is_none() {
            Reach::Window
        } else {
            Reach::Everywhere
        }
    }

    pub fn ask_permission() {}

    type OpenDisplay = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type QueryKeymap = unsafe extern "C" fn(*mut c_void, *mut c_char) -> c_int;
    type KeysymToKeycode = unsafe extern "C" fn(*mut c_void, c_ulong) -> c_uchar;
    type DefaultRootWindow = unsafe extern "C" fn(*mut c_void) -> c_ulong;
    type QueryPointer = unsafe extern "C" fn(*mut c_void, c_ulong, *mut c_ulong, *mut c_ulong, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_uint) -> c_int;

    /// Our own connection to the X server, used by the watching thread alone.
    pub struct Keys {
        display: *mut c_void,
        root: c_ulong,
        query_keymap: QueryKeymap,
        to_keycode: KeysymToKeycode,
        query_pointer: QueryPointer,
        /// Which keys were down at this poll, a bit each by keycode.
        keymap: [c_char; 32],
    }

    // The connection never leaves the watching thread once it is made there.
    unsafe impl Send for Keys {}

    impl Keys {
        pub fn open() -> Option<Keys> {
            // SAFETY: dlopen and dlsym with C strings; each symbol is checked before it is cast
            // to the type libX11 documents for it.
            unsafe {
                let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
                if lib.is_null() {
                    return None;
                }
                let symbol = |name: &std::ffi::CStr| {
                    let found = libc::dlsym(lib, name.as_ptr());
                    (!found.is_null()).then_some(found)
                };
                let open: OpenDisplay = std::mem::transmute(symbol(c"XOpenDisplay")?);
                let query_keymap: QueryKeymap = std::mem::transmute(symbol(c"XQueryKeymap")?);
                let to_keycode: KeysymToKeycode = std::mem::transmute(symbol(c"XKeysymToKeycode")?);
                let root_of: DefaultRootWindow = std::mem::transmute(symbol(c"XDefaultRootWindow")?);
                let query_pointer: QueryPointer = std::mem::transmute(symbol(c"XQueryPointer")?);
                let display = open(std::ptr::null());
                if display.is_null() {
                    return None;
                }
                Some(Keys { display, root: root_of(display), query_keymap, to_keycode, query_pointer, keymap: [0; 32] })
            }
        }

        /// The X keysym of a key (keysymdef.h).
        fn keysym(key: Key) -> Option<c_ulong> {
            Some(match key {
                Key::Letter(c) | Key::Digit(c) | Key::Punctuation(c) => c as c_ulong,
                Key::F(n) => 0xffbd + c_ulong::from(n),
                Key::Space => 0x20,
                Key::Tab => 0xff09,
                Key::Enter => 0xff0d,
                Key::Backspace => 0xff08,
                Key::Insert => 0xff63,
                Key::Delete => 0xffff,
                Key::Home => 0xff50,
                Key::Left => 0xff51,
                Key::Up => 0xff52,
                Key::Right => 0xff53,
                Key::Down => 0xff54,
                Key::PageUp => 0xff55,
                Key::PageDown => 0xff56,
                Key::End => 0xff57,
                Key::CapsLock => 0xffe5,
                _ => return None,
            })
        }

        /// Reads which keys are down, once a poll.
        pub fn poll(&mut self) {
            // SAFETY: the display is open and the buffer is the 32 bytes libX11 fills.
            unsafe { (self.query_keymap)(self.display, self.keymap.as_mut_ptr()) };
        }

        fn keysym_down(&self, keysym: c_ulong) -> bool {
            // SAFETY: the display is open; a keysym with no key on this keyboard gives 0.
            let code = unsafe { (self.to_keycode)(self.display, keysym) } as usize;
            code != 0 && self.keymap[code / 8] as u8 & (1 << (code % 8)) != 0
        }

        pub fn down(&mut self, key: Key) -> bool {
            match key {
                Key::Ctrl => self.keysym_down(0xffe3) || self.keysym_down(0xffe4),
                Key::Shift => self.keysym_down(0xffe1) || self.keysym_down(0xffe2),
                Key::Alt => self.keysym_down(0xffe9) || self.keysym_down(0xffea),
                Key::Meta => self.keysym_down(0xffeb) || self.keysym_down(0xffec),
                Key::Mouse3 => {
                    let (mut root, mut child, mut x, mut y, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0, 0, 0);
                    // SAFETY: the display is open and every out pointer is to a local.
                    unsafe { (self.query_pointer)(self.display, self.root, &mut root, &mut child, &mut x, &mut y, &mut wx, &mut wy, &mut mask) };
                    // Button2Mask: the middle button.
                    mask & (1 << 9) != 0
                }
                Key::Mouse4 | Key::Mouse5 => false,
                _ => Keys::keysym(key).is_some_and(|keysym| self.keysym_down(keysym)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_pressed_in_the_window_is_the_binding_it_names() {
        let stroke = |text: &str| Keystroke::parse(text).unwrap();
        assert_eq!(binding_of(&stroke("ctrl-shift-m")).map(|b| b.to_string()), Some("ctrl+shift+m".into()));
        assert_eq!(binding_of(&stroke("f13")).map(|b| b.to_string()), Some("f13".into()));
        assert_eq!(binding_of(&stroke("escape")), None, "escape cancels recording");
        assert_eq!(mouse_binding(MouseButton::Navigate(NavigationDirection::Back)), Some(Binding::new(Key::Mouse4)));
        assert_eq!(mouse_binding(MouseButton::Left), None);
        let ctrl = gpui::Modifiers { control: true, ..gpui::Modifiers::default() };
        assert_eq!(modifier_binding(ctrl), Some(Binding::new(Key::Ctrl)));
        assert_eq!(Action::ALL.map(Action::key), [PTT_KEY, PRIORITY_KEY, MUTE_KEY, DEAFEN_KEY]);
    }
}
