//! The Settings window (`docs/design/09-settings.md`, `docs/product/09-settings.md`): its own
//! 720 × 560 window, opened by ⌘, on the section last used. Every control acts at once and
//! changes real behaviour; what is not built is not in it.
//!
//! It holds nothing of its own beyond what it is drawing: chatd's settings are the shell's copy
//! (`prefs::values`), the window's own are `prefs::choices`, and the accounts are the shell's
//! snapshot, read each frame.

mod accounts;
pub(in crate::gui) mod controls;
mod notify;
mod pages;
pub mod rows;
mod tiny;

use super::app::prefs;
use super::app::service::Phase;
use super::components::{button, glyph, keycap, ButtonKind};
use super::input::{InputEvent, InputKey, TextInput};
use super::state::Shell;
use super::theme::{self, Type};
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, Context, Div, Entity, FocusHandle, Focusable, Global, KeyBinding, Pixels,
    SharedString, Subscription, Task, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions,
};
use protocol::{AccountStatus, Request, Response};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The window's size: fixed, not resizable.
const SIZE: (f32, f32) = (720., 560.);
const LIST_WIDTH: f32 = 184.;
/// A microphone test stops by itself after this.
const TEST_LIMIT: Duration = Duration::from_secs(30);
/// Keys move a slider at once; the value is stored this long after the last one.
const KEY_SETTLE: Duration = Duration::from_millis(300);

gpui::actions!(settings, [Find, Dismiss, NextControl, PreviousControl]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Accounts,
    General,
    Notifications,
    Sounds,
    Appearance,
    Messages,
    Privacy,
    Audio,
    Voice,
    Storage,
    Shortcuts,
    About,
}

impl Section {
    /// In list order; About is pinned to the bottom.
    pub const ALL: [Section; 12] = [
        Section::Accounts,
        Section::General,
        Section::Notifications,
        Section::Sounds,
        Section::Appearance,
        Section::Messages,
        Section::Privacy,
        Section::Audio,
        Section::Voice,
        Section::Storage,
        Section::Shortcuts,
        Section::About,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Section::Accounts => "Accounts",
            Section::General => "General",
            Section::Notifications => "Notifications",
            Section::Sounds => "Sounds",
            Section::Appearance => "Appearance",
            Section::Messages => "Messages",
            Section::Privacy => "Privacy",
            Section::Audio => "Audio & video",
            Section::Voice => "Voice",
            Section::Storage => "Storage",
            Section::Shortcuts => "Shortcuts",
            Section::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Accounts => "icons/users.svg",
            Section::General => "icons/settings.svg",
            Section::Notifications => "icons/bell.svg",
            Section::Sounds => "icons/volume-2.svg",
            Section::Appearance => "icons/sun-moon.svg",
            Section::Messages => "icons/message-square.svg",
            Section::Privacy => "icons/lock.svg",
            Section::Audio => "icons/mic.svg",
            Section::Voice => "icons/headphones.svg",
            Section::Storage => "icons/hard-drive.svg",
            Section::Shortcuts => "icons/command.svg",
            Section::About => "icons/info.svg",
        }
    }

    /// The name `shell.prefs` keeps it by. Never changed once shipped.
    pub const fn word(self) -> &'static str {
        match self {
            Section::Accounts => "accounts",
            Section::General => "general",
            Section::Notifications => "notifications",
            Section::Sounds => "sounds",
            Section::Appearance => "appearance",
            Section::Messages => "messages",
            Section::Privacy => "privacy",
            Section::Audio => "audio",
            Section::Voice => "voice",
            Section::Storage => "storage",
            Section::Shortcuts => "shortcuts",
            Section::About => "about",
        }
    }

    pub fn from_word(word: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.word() == word)
    }

    /// The section ⌘, opens: the one last shown, Accounts the first time.
    pub fn last() -> Section {
        prefs::choices().section
    }
}

/// What a modal is asking.
#[derive(Clone)]
pub(super) enum Confirm {
    /// Boxed: an account's status is ten times the size of the other questions.
    SignOut(Box<AccountStatus>),
    /// A shorter retention, with what it would delete when chatd said.
    Retention { days: u32, prunable: Option<(u64, u64)> },
    Clear,
    Reset,
    TinyEverywhere(Box<AccountStatus>),
}

/// Which device menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Picker {
    Input,
    Output,
}

/// What `mediad --devices` said: every device with its id, the defaults, the microphone
/// permission.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Devices {
    pub inputs: Vec<protocol::AudioDevice>,
    pub outputs: Vec<protocol::AudioDevice>,
    pub default_input: String,
    pub default_output: String,
    pub microphone: String,
}

impl Devices {
    pub fn listed(&self, picker: Picker) -> &[protocol::AudioDevice] {
        match picker {
            Picker::Input => &self.inputs,
            Picker::Output => &self.outputs,
        }
    }

    /// The device `choice` means now, as far as the list says. `None` when it is not listed.
    pub fn means(&self, choice: &protocol::settings::DeviceChoice, picker: Picker) -> Option<&protocol::AudioDevice> {
        use protocol::settings::DeviceChoice;
        let listed = self.listed(picker);
        match choice {
            DeviceChoice::Auto => listed.iter().find(|d| d.auto),
            DeviceChoice::System => listed.iter().find(|d| d.default),
            DeviceChoice::Device { .. } => listed.iter().find(|d| choice.is(d, listed)),
        }
    }
}

/// The shell the window reads, and the window once it is open.
#[derive(Default)]
struct Host {
    shell: Option<WeakEntity<Shell>>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for Host {}

/// The Settings window redraws. Safe to call while the shell is already being updated.
pub fn redraw(cx: &mut App) {
    let window = cx.default_global::<Host>().window;
    if let Some(window) = window {
        let _ = window.update(cx, |_, _, cx| cx.notify());
    }
}

/// The shell and the Settings window redraw after an update check.
pub fn note_changed(cx: &mut App) {
    let host = cx.default_global::<Host>();
    let window = host.window;
    let shell = host.shell.clone();
    if let Some(shell) = shell {
        let _ = shell.update(cx, |shell, cx| {
            if crate::update::announced() {
                shell.layout_state.update_alert = true;
            }
            shell.refresh_menus(cx);
            cx.notify();
        });
    }
    if let Some(window) = window {
        let _ = window.update(cx, |_, _, cx| cx.notify());
    }
}

/// Called by the shell when it starts: Settings reads its accounts from it.
pub fn attach(shell: WeakEntity<Shell>, cx: &mut App) {
    cx.default_global::<Host>().shell = Some(shell);
    cx.bind_keys([
        KeyBinding::new("secondary-f", Find, Some("Settings")),
        KeyBinding::new("escape", Dismiss, Some("Settings")),
        KeyBinding::new("tab", NextControl, Some("Settings")),
        KeyBinding::new("shift-tab", PreviousControl, Some("Settings")),
    ]);
}

/// The microphone test's level, from `Change::Meter`; `None` once it is over.
pub fn meter(level: Option<u8>, cx: &mut App) {
    let Some(window) = cx.default_global::<Host>().window else {
        return;
    };
    let _ = window.update(cx, |settings, _, cx| {
        settings.level = level;
        if level.is_none() {
            settings.testing = None;
        }
        cx.notify();
    });
}

/// Opens Settings on `section`, or brings the open window forward and shows `section` there.
/// Safe to call while the shell is already being updated: the shell's own buttons call it from
/// inside its update, and a new window reads the shell as it first draws.
pub fn open(section: Section, cx: &mut App) {
    cx.defer(move |cx| open_now(section, cx));
}

fn open_now(section: Section, cx: &mut App) {
    let host = cx.default_global::<Host>();
    let Some(shell) = host.shell.clone() else {
        return;
    };
    if let Some(window) = host.window {
        let shown = window.update(cx, |settings, window, cx| {
            settings.show(section, window, cx);
            window.activate_window();
        });
        if shown.is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(SIZE.0), px(SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(section.name().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(16.))),
        }),
        window_decorations: Some(gpui::WindowDecorations::Server),
        is_resizable: false,
        is_minimizable: false,
        app_id: Some(super::APP_ID.into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        // A microphone test and a key recorder end with the window.
        window.on_window_should_close(cx, |_, cx| {
            if let Some(window) = cx.default_global::<Host>().window {
                let _ = window.update(cx, |settings, _, cx| {
                    settings.stop_test(cx);
                    settings.stop_recording(cx);
                });
            }
            true
        });
        cx.new(|cx| SettingsWindow::new(shell, section, window, cx))
    });
    match opened {
        Ok(window) => cx.default_global::<Host>().window = Some(window),
        Err(err) => tracing::warn!("tinyclient: the Settings window could not open: {err}"),
    }
}

pub struct SettingsWindow {
    pub(super) shell: WeakEntity<Shell>,
    pub(super) section: Section,
    pub(super) confirm: Option<Confirm>,
    pub(super) picker: Option<Picker>,
    /// The device pickers' buttons, by `Picker`, so closing a menu gives the key back to its
    /// button; and the open menu, which takes ↑ ↓ Return Space and letters.
    pub(super) picker_focus: [FocusHandle; 2],
    pub(super) menu_focus: FocusHandle,
    /// The menu item the keys are on.
    pub(super) menu_cursor: usize,
    /// The section list: one Tab stop, ↑ and ↓ move through the sections.
    list_focus: FocusHandle,
    /// Where controls were drawn, for the sliders and the menus below their buttons.
    pub(super) anchors: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
    search: Entity<TextInput>,
    pub(super) query: String,
    focus: FocusHandle,
    /// The slider being dragged.
    pub(super) drag: Option<&'static str>,
    /// Slider values shown and not yet stored.
    pub(super) pending: HashMap<&'static str, u32>,
    settle: Option<Task<()>>,
    /// The microphone test, since when; and its level.
    pub(super) testing: Option<Instant>,
    pub(super) level: Option<u8>,
    test_timer: Option<Task<()>>,
    pub(super) devices: Option<Devices>,
    /// Bytes on this Mac: messages, files.
    pub(super) storage: Option<(u64, u64)>,
    /// The row whose value could not be stored, or whose action did not happen.
    pub(super) failed: Option<&'static str>,
    /// When the test sound was asked for, while it plays.
    pub(super) playing: Option<Instant>,
    /// "Copied", for 4 s after Copy diagnostics.
    pub(super) copied: Option<Instant>,
    /// Where a debug bundle was written, or that export is off. Shown for 4 s.
    pub(super) log_note: Option<(Instant, String)>,
    /// What the test notification came to, and when (N13).
    pub(super) tested: Option<(Instant, String)>,
    /// A row opened from search, lit for 1.5 s.
    pub(super) highlight: Option<&'static str>,
    /// The Tiny server account's name field.
    tiny: tiny::TinyAccount,
    /// The key recorder taking the next key (V13, V15 to V17), and what has the keys while it
    /// does. The modifiers held so far with no other key, for a modifier bound on its own.
    pub(super) recording: Option<&'static str>,
    pub(super) recorder_focus: FocusHandle,
    pub(super) held_modifiers: gpui::Modifiers,
    /// A key that is already another action's, refused, and the words that say so.
    pub(super) clash: Option<(&'static str, String)>,
    /// V18: the microphone test also plays the microphone back. The window's alone.
    pub(super) loopback: bool,
    /// The section's pane, so a shot can draw it scrolled (`--settings-scroll`).
    #[cfg(feature = "gui-shot")]
    pane_scroll: gpui::ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    fn new(shell: WeakEntity<Shell>, section: Section, window: &mut Window, cx: &mut Context<Self>) -> SettingsWindow {
        let search = cx.new(|cx| TextInput::new("Search settings", cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Changed = event {
                    this.query = input.read(cx).text().to_owned();
                    this.picker = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event: &InputKey, window, cx| {
                if *event == InputKey::Escape {
                    this.dismiss(window, cx);
                }
            }),
        ];
        let (tiny, typed) = tiny::TinyAccount::new(window, cx);
        subscriptions.extend(typed);
        // The shell's snapshot changing is what redraws the accounts and the call's meter.
        if let Some(shell) = shell.upgrade() {
            subscriptions.push(cx.observe(&shell, |this, _, cx| {
                if this.section == Section::Accounts {
                    this.read_tiny_names(cx);
                }
                cx.notify();
            }));
        }
        let mut this = SettingsWindow {
            shell,
            section,
            confirm: None,
            picker: None,
            picker_focus: [cx.focus_handle().tab_index(0).tab_stop(true), cx.focus_handle().tab_index(0).tab_stop(true)],
            menu_focus: cx.focus_handle(),
            menu_cursor: 0,
            list_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            anchors: Rc::default(),
            search,
            query: String::new(),
            focus: cx.focus_handle(),
            drag: None,
            pending: HashMap::new(),
            settle: None,
            testing: None,
            level: None,
            test_timer: None,
            devices: None,
            storage: None,
            failed: None,
            playing: None,
            copied: None,
            log_note: None,
            tested: None,
            highlight: None,
            tiny,
            recording: None,
            recorder_focus: cx.focus_handle(),
            held_modifiers: gpui::Modifiers::default(),
            clash: None,
            loopback: false,
            #[cfg(feature = "gui-shot")]
            pane_scroll: gpui::ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        window.focus(&this.focus, cx);
        this.entered(section, cx);
        this
    }

    /// Shows `section`: the test stops, what the section reads is read, and the window is named
    /// for it.
    pub(super) fn show(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.section != section {
            self.stop_test(cx);
            self.stop_recording(cx);
        }
        self.section = section;
        self.close_modal(cx);
        self.picker = None;
        self.highlight = None;
        window.set_window_title(section.name());
        self.entered(section, cx);
        let mut choices = prefs::choices();
        if choices.section != section {
            choices.section = section;
            prefs::set_choices(choices);
            let _ = self.shell.update(cx, |shell, cx| shell.save_prefs(cx));
        }
        cx.notify();
    }

    /// Closes the question on show.
    pub(super) fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        cx.notify();
    }

    fn entered(&mut self, section: Section, cx: &mut Context<Self>) {
        match section {
            Section::Audio => self.list_devices(cx),
            Section::Storage => self.read_storage(cx),
            Section::Accounts => {
                self.tiny.forget_names();
                self.read_tiny_names(cx);
            }
            _ => {}
        }
    }

    /// Whether chatd answers, so its settings can be changed.
    pub(super) fn reachable(&self, cx: &App) -> bool {
        prefs::values_known() && self.shell.upgrade().is_some_and(|shell| shell.read(cx).service.phase == Phase::Live)
    }

    /// Runs `work` off the main thread and hands its answer back to the window.
    pub(super) fn off_thread<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        then: impl FnOnce(&mut SettingsWindow, R, &mut Context<SettingsWindow>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| then(this, answer, cx));
        })
        .detach();
    }

    /// Stores one of chatd's settings: the one gate every row chatd keeps goes through. While
    /// chatd cannot be reached nothing changes, as the rows are drawn disabled. Otherwise the
    /// window shows the new value at once; if it is not saved, the value from before comes back
    /// whether or not chatd answers again, and the row says so.
    pub(super) fn set(&mut self, row: &'static str, scope: String, key: &'static str, value: Option<String>, cx: &mut Context<Self>) {
        if !self.reachable(cx) {
            return;
        }
        let mut values = prefs::values();
        let before = values.stored(&scope, key).map(str::to_owned);
        values.set(&scope, key, value.clone());
        prefs::set_values(values);
        self.failed = None;
        self.shown_everywhere(cx);
        let request = Request::Set { scope: scope.clone(), key: key.to_owned(), value: value.clone() };
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                // Only this row goes back, and only if nothing changed it since.
                let mut values = prefs::values();
                if values.stored(&scope, key) == value.as_deref() {
                    values.set(&scope, key, before);
                    prefs::set_values(values);
                }
                this.failed = Some(row);
                this.shown_everywhere(cx);
                let _ = this.shell.update(cx, |shell, cx| shell.load_settings(cx));
            }
            cx.notify();
        });
    }

    /// chatd's settings changed here: the main window and the call's menu read them too.
    fn shown_everywhere(&mut self, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Whether row `id` is one chatd keeps, and so waits on chatd.
    pub(super) fn waits_on_chatd(&self, id: &str, cx: &App) -> bool {
        rows::kept_by_chatd(id) && !self.reachable(cx)
    }

    /// Changes one of the window's own settings and saves it. Every window draws it at once.
    pub(super) fn choose(&mut self, change: impl FnOnce(&mut prefs::Choices), cx: &mut Context<Self>) {
        let mut choices = prefs::choices();
        change(&mut choices);
        prefs::set_choices(choices);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.save_prefs(cx);
            cx.notify();
        });
        cx.refresh_windows();
    }

    /// M5: when GIFs play, in effect at the next draw of every window.
    pub(super) fn set_autoplay(&mut self, autoplay: crate::gui::anim::Autoplay, cx: &mut Context<Self>) {
        crate::gui::anim::set_autoplay(autoplay);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.save_prefs(cx);
            cx.notify();
        });
        cx.refresh_windows();
    }

    pub(super) fn set_text_step(&mut self, step: i32, cx: &mut Context<Self>) {
        theme::set_text_step(step);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.save_prefs(cx);
            cx.notify();
        });
        cx.refresh_windows();
    }

    /// A slider's value moved by the pointer.
    pub(super) fn slide_to(&mut self, id: &'static str, x: Pixels, cx: &mut Context<Self>) {
        if self.waits_on_chatd(id, cx) {
            return;
        }
        let Some(bounds) = self.anchors.borrow().get(id).copied() else {
            return;
        };
        let spec = pages::slider(id);
        let value = controls::value_at(f32::from(x - bounds.origin.x), spec.range, 1, f32::from(bounds.size.width));
        if self.pending.get(id) != Some(&value) {
            self.pending.insert(id, value);
            if id == "A2" {
                // Text size is seen as it moves.
                self.set_text_step(value as i32 - 1, cx);
            }
            cx.notify();
        }
    }

    /// Stores a slider's value: when the pointer lets go, or a moment after the last key.
    pub(super) fn commit(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let Some(value) = self.pending.remove(id) else {
            return;
        };
        pages::store_slider(self, id, value, cx);
    }

    /// Arrow keys on a focused slider: one step, ten with ⌥, Home and End to the ends.
    pub(super) fn slider_key(&mut self, id: &'static str, key: &str, alt: bool, cx: &mut Context<Self>) -> bool {
        if self.waits_on_chatd(id, cx) {
            return false;
        }
        let spec = pages::slider(id);
        let now = self.pending.get(id).copied().unwrap_or_else(|| (spec.read)());
        let by = if alt { 10 } else { 1 };
        let next = match key {
            "left" | "down" => now.saturating_sub(by).max(spec.range.0),
            "right" | "up" => (now + by).min(spec.range.1),
            "home" => spec.range.0,
            "end" => spec.range.1,
            _ => return false,
        };
        self.pending.insert(id, next);
        if id == "A2" {
            self.set_text_step(next as i32 - 1, cx);
        }
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(KEY_SETTLE).await;
            let _ = this.update(cx, |this, cx| this.commit(id, cx));
        }));
        cx.notify();
        true
    }

    /// Starts or stops the microphone test.
    pub(super) fn toggle_test(&mut self, cx: &mut Context<Self>) {
        if self.testing.is_some() {
            self.stop_test(cx);
            cx.notify();
            return;
        }
        self.testing = Some(Instant::now());
        self.level = None;
        let loopback = self.loopback;
        self.off_thread(cx, move || crate::client::ask(&Request::TestMicrophone { on: true, loopback }), |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                this.testing = None;
                this.failed = Some("V3");
            }
            cx.notify();
        });
        self.test_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TEST_LIMIT).await;
            let _ = this.update(cx, |this, cx| this.stop_test(cx));
        }));
        cx.notify();
    }

    pub(super) fn stop_test(&mut self, cx: &mut Context<Self>) {
        if self.testing.take().is_none() {
            return;
        }
        self.level = None;
        self.test_timer = None;
        self.off_thread(cx, || crate::client::ask(&Request::TestMicrophone { on: false, loopback: false }), |_, _, _| {});
    }

    /// V18: the test plays the microphone back, or stops. A test running starts again with it.
    pub(super) fn set_loopback(&mut self, on: bool, cx: &mut Context<Self>) {
        self.loopback = on;
        if self.testing.is_some() {
            let loopback = on;
            self.off_thread(cx, move || crate::client::ask(&Request::TestMicrophone { on: true, loopback }), |_, _, _| {});
        }
        cx.notify();
    }

    /// A key recorder takes the next key pressed, or mouse button. Nothing else hears it: the
    /// global keys stop meanwhile.
    pub(super) fn start_recording(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reachable(cx) {
            return;
        }
        if super::hotkeys::reach() == super::hotkeys::Reach::Permission {
            super::hotkeys::ask_permission();
        }
        self.recording = Some(id);
        self.held_modifiers = gpui::Modifiers::default();
        self.clash = None;
        super::hotkeys::pause(true);
        window.focus(&self.recorder_focus, cx);
        cx.notify();
    }

    pub(super) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            super::hotkeys::pause(false);
            cx.notify();
        }
    }

    /// The recorder heard `binding` (`None`: cleared). Taken unless another key action has it.
    pub(super) fn recorded(&mut self, binding: Option<protocol::hotkey::Binding>, cx: &mut Context<Self>) {
        let Some(id) = self.recording else {
            return;
        };
        self.stop_recording(cx);
        let Some(action) = pages::key_action(id) else {
            return;
        };
        let values = prefs::values();
        if let Some(binding) = binding {
            let taken = super::hotkeys::Action::ALL.into_iter().filter(|other| *other != action).find(|other| values.hotkey(other.key()) == Some(binding));
            if let Some(other) = taken {
                self.clash = Some((id, format!("{} is already {}.", binding.label(), other.name())));
                cx.notify();
                return;
            }
        }
        self.set(id, protocol::settings::GLOBAL.into(), action.key(), binding.map(|b| b.to_string()), cx);
    }

    /// Lists the devices with `mediad --devices`, which opens nothing and exits.
    pub(super) fn list_devices(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, read_devices, |this, devices, cx| {
            if devices.is_some() && this.devices != devices {
                this.devices = devices;
                this.learn_devices(cx);
                cx.notify();
            }
        });
    }

    /// A device chosen by name alone (as builds before ids stored it) is stored again with its
    /// id once it is listed, so it is found when Windows renames it.
    fn learn_devices(&mut self, cx: &mut Context<Self>) {
        let Some(devices) = self.devices.clone() else {
            return;
        };
        for (row, key, picker) in [("V1", protocol::settings::INPUT, Picker::Input), ("V2", protocol::settings::OUTPUT, Picker::Output)] {
            if let Some(better) = prefs::values().device_choice(key).upgrade(devices.listed(picker)) {
                self.set(row, protocol::settings::GLOBAL.into(), key, better.value(), cx);
            }
        }
    }

    pub(super) fn read_storage(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || crate::client::ask(&Request::Storage), |this, answer, cx| {
            if let Ok(Response::Storage { messages, files }) = answer {
                this.storage = Some((messages, files));
                cx.notify();
            }
        });
    }

    /// Opens a device menu with the keys on the item that is checked.
    pub(super) fn open_picker(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_cursor = self.picker_checked(picker);
        self.picker = Some(picker);
        window.focus(&self.menu_focus, cx);
        // The list is read again as the menu opens: a device may have come or gone.
        self.list_devices(cx);
        cx.notify();
    }

    /// Closes the device menu and gives the keys back to its button.
    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.picker.take() {
            if self.menu_focus.is_focused(window) {
                window.focus(&self.picker_focus[picker as usize], cx);
            }
        }
        cx.notify();
    }

    pub(super) fn pick_device(&mut self, picker: Picker, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(window, cx);
        let (row, key) = match picker {
            Picker::Input => ("V1", protocol::settings::INPUT),
            Picker::Output => ("V2", protocol::settings::OUTPUT),
        };
        self.set(row, protocol::settings::GLOBAL.into(), key, name, cx);
    }

    /// A key in an open device menu: ↑ and ↓ move, Return and Space pick, a letter goes to
    /// the next item that starts with it. Esc and Tab are the window's (`dismiss`).
    pub(super) fn menu_key(&mut self, picker: Picker, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let choices = self.picker_choices(picker);
        let last = choices.len().saturating_sub(1);
        let modified = keystroke.modifiers.platform || keystroke.modifiers.control || keystroke.modifiers.alt;
        match keystroke.key.as_str() {
            "up" => self.menu_cursor = self.menu_cursor.saturating_sub(1),
            "down" => self.menu_cursor = (self.menu_cursor + 1).min(last),
            "home" => self.menu_cursor = 0,
            "end" => self.menu_cursor = last,
            "enter" | "space" => {
                if let Some((value, words)) = choices.into_iter().nth(self.menu_cursor) {
                    if !words.starts_with("No ") {
                        self.pick_device(picker, value, window, cx);
                    }
                }
                return true;
            }
            key if !modified && key.chars().count() == 1 => {
                let letter = key.to_lowercase();
                let next = (1..=choices.len()).map(|step| (self.menu_cursor + step) % choices.len()).find(|ix| choices[*ix].1.to_lowercase().starts_with(&letter));
                match next {
                    Some(ix) => self.menu_cursor = ix,
                    None => return false,
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// ↑ and ↓ on the section list: the section above or below, About last.
    fn list_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let at = Section::ALL.iter().position(|s| *s == self.section).unwrap_or(0);
        let next = match key {
            "up" => at.checked_sub(1),
            "down" => Some(at + 1).filter(|ix| *ix < Section::ALL.len()),
            "home" => Some(0),
            "end" => Some(Section::ALL.len() - 1),
            _ => return false,
        };
        if let Some(section) = next.and_then(|ix| Section::ALL.get(ix).copied()) {
            if !self.query.is_empty() {
                self.query.clear();
                self.search.update(cx, |input, cx| input.set_text("", cx));
            }
            self.show(section, window, cx);
        }
        true
    }

    /// Esc: closes a menu or modal, then clears the search, then closes the window.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.close_picker(window, cx);
        } else if self.confirm.is_some() {
            self.close_modal(cx);
        } else if self.recording.is_some() {
            self.stop_recording(cx);
        } else if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
        } else {
            self.stop_test(cx);
            self.stop_recording(cx);
            window.remove_window();
        }
    }

    /// Opens a search result's section with its row lit for 1.5 s.
    pub(super) fn reveal(&mut self, row: &'static rows::Row, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.show(row.section, window, cx);
        self.highlight = Some(row.id);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                this.highlight = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn list(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let searching = !self.query.trim().is_empty();
        // The list is one Tab stop (§11): the ring is on the selected section while it has the
        // keys, and ↑ ↓ move the selection.
        let list_focused = self.list_focus.is_focused(window);
        let item = |section: Section, cx: &mut Context<Self>| {
            let selected = self.section == section;
            div()
                .id(section.name())
                .h(px(28.))
                .flex_none()
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .role(gpui::Role::Tab)
                .aria_label(section.name())
                .aria_selected(selected)
                .type_ui()
                .text_color(if selected { theme::text() } else { theme::text_soft() })
                .when(selected, |item| item.bg(theme::selected()))
                .when(selected && list_focused, |item| item.shadow(theme::focus_ring_inset()))
                .when(!selected, |item| item.hover(|style| style.bg(theme::hover())))
                .child(glyph(section.icon(), 16., if selected { theme::text() } else { theme::text_faint() }))
                .child(section.name())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.query.is_empty() {
                        this.query.clear();
                        this.search.update(cx, |input, cx| input.set_text("", cx));
                    }
                    this.show(section, window, cx);
                }))
        };
        let focused = self.search.focus_handle(cx).is_focused(window);
        let field = super::components::search_field(focused)
            .w(px(168.))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            // Past +1 the placeholder and the keycap do not both fit; the placeholder stays.
            .when(!searching && !focused && theme::text_step() <= 1, |field| field.child(keycap("⌘F")))
            .when(searching, |field| {
                field.child(
                    div()
                        .id("search-clear")
                        .cursor_pointer()
                        .child(glyph("icons/x.svg", 14., theme::text_faint()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.query.clear();
                            this.search.update(cx, |input, cx| input.set_text("", cx));
                            cx.notify();
                        })),
                )
            });
        let mut list = div()
            .w(px(LIST_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .px(px(8.))
            .pt(px(theme::STRIP))
            .pb(px(12.))
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::hairline())
            .child(field)
            .child(div().h(px(8.)).flex_none());
        let mut items = div().flex().flex_col().gap(px(2.));
        for section in Section::ALL.into_iter().filter(|s| *s != Section::About) {
            items = items.child(item(section, cx));
        }
        let sections = div()
            .id("sections")
            .track_focus(&self.list_focus)
            .role(gpui::Role::TabList)
            .flex_1()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.list_key(event.keystroke.key.as_str(), window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(items)
            .child(div().flex_1())
            .child(item(Section::About, cx));
        list = list.child(sections);
        list
    }

    fn pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let searching = !self.query.trim().is_empty();
        let heading = if searching { "Results" } else { self.section.name() };
        let body = if searching { self.results(window, cx) } else { self.page(window, cx) };
        let scroller = div().id(SharedString::from(format!("pane-{}", self.section.name()))).flex_1().min_h_0().overflow_y_scroll();
        #[cfg(feature = "gui-shot")]
        let scroller = scroller.track_scroll(&self.pane_scroll);
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .child(
                div()
                    .h(px(theme::STRIP))
                    .flex_none()
                    .pl(px(28.))
                    .flex()
                    .items_center()
                    .type_title()
                    .text_color(theme::text())
                    .child(heading),
            )
            .child(scroller.child(div().w(px(controls::COLUMN + 56.)).px(px(28.)).pb(px(28.)).child(body)))
    }
}

/// `TINYCLIENT_MEDIAD`, or the `mediad` beside this window, as chatd finds it.
fn mediad_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("TINYCLIENT_MEDIAD") {
        return path.into();
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.with_file_name(format!("mediad{}", std::env::consts::EXE_SUFFIX))
}

/// The system's devices, by the names a call takes. `None` when mediad could not say.
fn read_devices() -> Option<Devices> {
    let output = protocol::launch::no_console(std::process::Command::new(mediad_path()).arg("--devices")).stderr(std::process::Stdio::null()).output().ok()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    // With ids; a mediad from before ids had names alone, which stand in for both.
    let devices = |key: &str, names: &str| -> Vec<protocol::AudioDevice> {
        match serde_json::from_value::<Vec<protocol::AudioDevice>>(json[key].clone()) {
            Ok(devices) => devices.into_iter().take(protocol::DEVICES_LIMIT).collect(),
            Err(_) => json[names]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|name| name.as_str())
                .take(protocol::DEVICES_LIMIT)
                .map(|name| protocol::AudioDevice { id: name.to_owned(), name: name.to_owned(), ..Default::default() })
                .collect(),
        }
    };
    let text = |key: &str| json[key].as_str().unwrap_or_default().to_owned();
    Some(Devices {
        inputs: devices("input_devices", "inputs"),
        outputs: devices("output_devices", "outputs"),
        default_input: text("default_input"),
        default_output: text("default_output"),
        microphone: text("microphone"),
    })
}

/// A modal: title, sentences, Cancel and the action, over a scrim that cancels on a click.
pub(super) fn modal_panel(title: impl Into<SharedString>, words: Vec<String>, action: gpui::Stateful<Div>, cx: &mut Context<SettingsWindow>) -> Div {
    let panel = super::components::modal(title)
        .children(words.into_iter().map(|line| div().mt(px(8.)).type_ui().text_color(theme::text_soft()).child(line)))
        .child(
            div()
                .mt(px(20.))
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(button("modal-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
                .child(action),
        );
    modal_layer(panel, cx)
}

/// A panel over the whole window. A click on the scrim closes it.
pub(super) fn modal_layer(panel: Div, cx: &mut Context<SettingsWindow>) -> Div {
    div()
        .absolute()
        .inset_0()
        .child(
            div()
                .id("modal-scrim")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(div().id("modal").occlude().on_click(|_, _, cx| cx.stop_propagation()).child(panel)),
        )
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modal = self.confirm.clone().map(|confirm| self.modal(&confirm, cx));
        let menu = self.picker.and_then(|picker| self.device_menu(picker, window, cx));
        let toast = self.log_note.as_ref().filter(|(at, _)| at.elapsed() < Duration::from_secs(4)).map(|(_, note)| {
            div().absolute().bottom(px(16.)).left_0().right_0().flex().justify_center().child(
                super::components::toast(Some(("icons/check.svg", theme::text_soft())), note.clone()).w(px(360.)),
            )
        }).or_else(|| {
            self.copied.filter(|at| at.elapsed() < Duration::from_secs(4)).map(|_| {
                div().absolute().bottom(px(16.)).left_0().right_0().flex().justify_center().child(
                    super::components::toast(Some(("icons/check.svg", theme::text_soft())), "Copied").w(px(200.)),
                )
            })
        });
        macro_rules! section_keys {
            ($root:expr, $($action:ident => $ix:expr),*) => {
                $root$(.on_action(cx.listener(|this, _: &super::$action, window, cx| {
                    if let Some(section) = Section::ALL.get($ix).copied() {
                        this.show(section, window, cx);
                    }
                })))*
            };
        }
        let root = div()
            .size_full()
            .relative()
            .flex()
            .bg(theme::canvas())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                let handle = this.search.focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &NextControl, window, cx| {
                this.close_picker(window, cx);
                window.focus_next(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousControl, window, cx| {
                this.close_picker(window, cx);
                window.focus_prev(cx);
            }))
            .on_action(cx.listener(|this, _: &super::CloseWindow, window, cx| {
                this.stop_test(cx);
                this.stop_recording(cx);
                window.remove_window();
            }))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if let (Some(id), true) = (this.drag, event.dragging()) {
                    this.slide_to(id, event.position.x, cx);
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(id) = this.drag.take() {
                        this.commit(id, cx);
                    }
                }),
            );
        // A key recorder takes the middle and side buttons anywhere in the window.
        let root = [gpui::MouseButton::Middle, gpui::MouseButton::Navigate(gpui::NavigationDirection::Back), gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward)]
            .into_iter()
            .fold(root, |root, button| {
                root.on_mouse_down(
                    button,
                    cx.listener(move |this, _, _, cx| {
                        if this.recording.is_some() {
                            this.recorded(super::hotkeys::mouse_binding(button), cx);
                        }
                    }),
                )
            });
        let root = section_keys!(root, Place1 => 0, Place2 => 1, Place3 => 2, Place4 => 3, Place5 => 4, Place6 => 5, Place7 => 6, Place8 => 7, Place9 => 8);
        // The top strip moves the window: drawn first, it takes only the presses nothing above it does.
        let strip = super::layout::drags_window(div().absolute().top_0().left_0().w_full().h(px(theme::STRIP)));
        let close = super::chrome::buttons(
            window,
            cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                this.stop_test(cx);
                this.stop_recording(cx);
                window.remove_window();
            }),
        );
        root.child(strip)
            .child(self.list(window, cx))
            .child(self.pane(window, cx))
            .children(menu)
            .children(toast)
            .children(modal)
            .children(close)
    }
}

/// `--settings <section> --settings-shot out.png`, with `--shot`: opens Settings on a section
/// once the shell has its first answer and writes that window's frame. `--settings-search
/// <words>` types into the search; `--settings-confirm sign-out|clear|reset|retention|tiny-everywhere` opens
/// that question. `--settings-scroll <points>` draws the section scrolled down that far.
#[cfg(feature = "gui-shot")]
pub fn shot(cx: &mut App) {
    let flag = |name: &str| std::env::args().skip_while(|arg| arg != name).nth(1);
    let (Some(section), Some(path)) = (flag("--settings"), flag("--settings-shot")) else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
        let open_on = Section::ALL.into_iter().find(|s| s.name().to_lowercase().starts_with(&section.to_lowercase())).unwrap_or(Section::Accounts);
        // From inside the shell's update, as the shell's own buttons open it.
        cx.update(|cx| {
            if let Some(shell) = cx.default_global::<Host>().shell.clone() {
                let _ = shell.update(cx, |_, cx| open(open_on, cx));
            }
        });
        let Some(window) = cx.update(|cx| cx.default_global::<Host>().window) else {
            return;
        };
        let _ = window.update(cx, |settings, _, cx| {
            if let Some(words) = flag("--settings-search") {
                settings.query = words.clone();
                settings.search.update(cx, |input, cx| input.set_text(&words, cx));
            }
            settings.confirm = match flag("--settings-confirm").as_deref() {
                Some("sign-out") => settings.shell.upgrade().and_then(|shell| shell.read(cx).snapshot.as_ref()?.accounts.first().cloned()).map(|account| Confirm::SignOut(Box::new(account))),
                Some("tiny-everywhere") => settings
                    .shell
                    .upgrade()
                    .and_then(|shell| shell.read(cx).snapshot.as_ref()?.accounts.iter().find(|a| a.key.network == protocol::Network::Tiny).cloned())
                    .map(|account| Confirm::TinyEverywhere(Box::new(account))),
                Some("clear") => Some(Confirm::Clear),
                Some("reset") => Some(Confirm::Reset),
                Some("retention") => Some(Confirm::Retention { days: 7, prunable: Some((41_210, 12 * 1024 * 1024)) }),
                _ => None,
            };
            if flag("--settings-menu").as_deref() == Some("input") {
                settings.picker = Some(Picker::Input);
            }
            if let Some(down) = flag("--settings-scroll").and_then(|n| n.parse::<f32>().ok()) {
                settings.pane_scroll.set_offset(point(px(0.), px(-down)));
            }
            cx.notify();
        });
        // Pictures and answers land on the first frames; draw again once they have.
        for _ in 0..6 {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            let _ = window.update(cx, |_, window, _| window.refresh());
        }
        cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
        match window.update(cx, |_, window, _| window.render_to_image()) {
            Ok(Ok(image)) => image.save(&path).expect("the Settings shot is written"),
            _ => tracing::warn!("tinyclient: no Settings shot"),
        }
    })
    .detach();
}
