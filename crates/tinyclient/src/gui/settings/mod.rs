//! The Settings window (`docs/design/09-settings.md`, `docs/product/09-settings.md`): its own
//! 720 × 560 window, opened by ⌘, on the section last used. Every control acts at once and
//! changes real behaviour; what is not built is not in it.
//!
//! It holds nothing of its own beyond what it is drawing: chatd's settings are the shell's copy
//! (`prefs::values`), the window's own are `prefs::choices`, and the accounts are the shell's
//! snapshot, read each frame.

mod accounts;
mod controls;
mod pages;
pub mod rows;

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
    Sounds,
    Appearance,
    Messages,
    Privacy,
    Audio,
    Storage,
    Shortcuts,
    About,
}

impl Section {
    /// In list order; About is pinned to the bottom.
    pub const ALL: [Section; 10] = [
        Section::Accounts,
        Section::General,
        Section::Sounds,
        Section::Appearance,
        Section::Messages,
        Section::Privacy,
        Section::Audio,
        Section::Storage,
        Section::Shortcuts,
        Section::About,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Section::Accounts => "Accounts",
            Section::General => "General",
            Section::Sounds => "Sounds",
            Section::Appearance => "Appearance",
            Section::Messages => "Messages",
            Section::Privacy => "Privacy",
            Section::Audio => "Audio & video",
            Section::Storage => "Storage",
            Section::Shortcuts => "Shortcuts",
            Section::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Accounts => "icons/users.svg",
            Section::General => "icons/settings.svg",
            Section::Sounds => "icons/volume-2.svg",
            Section::Appearance => "icons/sun-moon.svg",
            Section::Messages => "icons/message-square.svg",
            Section::Privacy => "icons/lock.svg",
            Section::Audio => "icons/mic.svg",
            Section::Storage => "icons/hard-drive.svg",
            Section::Shortcuts => "icons/command.svg",
            Section::About => "icons/info.svg",
        }
    }

    fn index(self) -> u8 {
        Section::ALL.iter().position(|s| *s == self).unwrap_or(0) as u8
    }

    /// The section ⌘, opens: the one last shown, Accounts the first time.
    pub fn last() -> Section {
        Section::ALL.get(usize::from(prefs::choices().section)).copied().unwrap_or(Section::Accounts)
    }
}

/// What a modal is asking.
#[derive(Clone)]
pub(super) enum Confirm {
    SignOut(AccountStatus),
    /// A shorter retention, with what it would delete when chatd said.
    Retention { days: u32, prunable: Option<(u64, u64)> },
    Clear,
    Reset,
}

/// Which device menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Picker {
    Input,
    Output,
}

/// What `mediad --devices` said: every device by name, the defaults, the microphone permission.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Devices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: String,
    pub default_output: String,
    pub microphone: String,
}

/// The shell the window reads, and the window once it is open.
#[derive(Default)]
struct Host {
    shell: Option<WeakEntity<Shell>>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for Host {}

/// Called by the shell when it starts: Settings reads its accounts from it.
pub fn attach(shell: WeakEntity<Shell>, cx: &mut App) {
    cx.default_global::<Host>().shell = Some(shell);
    cx.bind_keys([
        KeyBinding::new("cmd-f", Find, Some("Settings")),
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
pub fn open(section: Section, cx: &mut App) {
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
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        // A microphone test ends with the window.
        window.on_window_should_close(cx, |_, cx| {
            if let Some(window) = cx.default_global::<Host>().window {
                let _ = window.update(cx, |settings, _, cx| settings.stop_test(cx));
            }
            true
        });
        cx.new(|cx| SettingsWindow::new(shell, section, window, cx))
    });
    match opened {
        Ok(window) => cx.default_global::<Host>().window = Some(window),
        Err(err) => eprintln!("tinyclient: the Settings window could not open: {err}"),
    }
}

pub struct SettingsWindow {
    pub(super) shell: WeakEntity<Shell>,
    pub(super) section: Section,
    pub(super) confirm: Option<Confirm>,
    pub(super) picker: Option<Picker>,
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
    /// The row whose value could not be stored.
    pub(super) failed: Option<&'static str>,
    /// "Copied", for 4 s after Copy diagnostics.
    pub(super) copied: Option<Instant>,
    /// A row opened from search, lit for 1.5 s.
    pub(super) highlight: Option<&'static str>,
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
        // The shell's snapshot changing is what redraws the accounts and the call's meter.
        if let Some(shell) = shell.upgrade() {
            subscriptions.push(cx.observe(&shell, |_, _, cx| cx.notify()));
        }
        let mut this = SettingsWindow {
            shell,
            section,
            confirm: None,
            picker: None,
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
            copied: None,
            highlight: None,
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
        }
        self.section = section;
        self.confirm = None;
        self.picker = None;
        self.highlight = None;
        window.set_window_title(section.name());
        self.entered(section, cx);
        let mut choices = prefs::choices();
        if choices.section != section.index() {
            choices.section = section.index();
            prefs::set_choices(choices);
            let _ = self.shell.update(cx, |shell, cx| shell.save_prefs(cx));
        }
        cx.notify();
    }

    fn entered(&mut self, section: Section, cx: &mut Context<Self>) {
        match section {
            Section::Audio => self.list_devices(cx),
            Section::Storage => self.read_storage(cx),
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

    /// Stores one of chatd's settings. The window shows the new value at once; if chatd refuses
    /// it, the stored one comes back and the row says so.
    pub(super) fn set(&mut self, row: &'static str, scope: String, key: &'static str, value: Option<String>, cx: &mut Context<Self>) {
        let mut values = prefs::values();
        values.set(&scope, key, value.clone());
        prefs::set_values(values);
        self.failed = None;
        cx.notify();
        let request = Request::Set { scope, key: key.to_owned(), value };
        self.off_thread(cx, move || crate::client::ask(&request), move |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                this.failed = Some(row);
                let _ = this.shell.update(cx, |shell, cx| shell.load_settings(cx));
            }
            cx.notify();
        });
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
        self.off_thread(cx, || crate::client::ask(&Request::TestMicrophone { on: true }), |this, answer, cx| {
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
        self.off_thread(cx, || crate::client::ask(&Request::TestMicrophone { on: false }), |_, _, _| {});
    }

    /// Lists the devices with `mediad --devices`, which opens nothing and exits.
    pub(super) fn list_devices(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, read_devices, |this, devices, cx| {
            if devices.is_some() && this.devices != devices {
                this.devices = devices;
                cx.notify();
            }
        });
    }

    pub(super) fn read_storage(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || crate::client::ask(&Request::Storage), |this, answer, cx| {
            if let Ok(Response::Storage { messages, files }) = answer {
                this.storage = Some((messages, files));
                cx.notify();
            }
        });
    }

    /// Esc: closes a menu or modal, then clears the search, then closes the window.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() || self.confirm.take().is_some() {
            cx.notify();
        } else if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
        } else {
            self.stop_test(cx);
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
                .tab_index(0)
                .role(gpui::Role::Tab)
                .aria_label(section.name())
                .aria_selected(selected)
                .type_ui()
                .text_color(if selected { theme::text() } else { theme::text_soft() })
                .when(selected, |item| item.bg(theme::selected()))
                .when(!selected, |item| item.hover(|style| style.bg(theme::hover())))
                .focus_visible(|style| style.shadow(theme::focus_ring_inset()))
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
            .when(!searching && !focused, |field| field.child(keycap("⌘F")))
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
        let mut items = div().id("sections").flex().flex_col().gap(px(2.)).role(gpui::Role::TabList);
        for section in Section::ALL.into_iter().filter(|s| *s != Section::About) {
            items = items.child(item(section, cx));
        }
        list = list.child(items).child(div().flex_1()).child(item(Section::About, cx));
        list
    }

    fn pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let searching = !self.query.trim().is_empty();
        let heading = if searching { "Results" } else { self.section.name() };
        let body = if searching { self.results(window, cx) } else { self.page(window, cx) };
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
            .child(
                div()
                    .id(SharedString::from(format!("pane-{}", self.section.name())))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().w(px(controls::COLUMN + 56.)).px(px(28.)).pb(px(28.)).child(body)),
            )
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
    let output = std::process::Command::new(mediad_path()).arg("--devices").stderr(std::process::Stdio::null()).output().ok()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let names = |key: &str| -> Vec<String> {
        json[key].as_array().into_iter().flatten().filter_map(|name| name.as_str().map(str::to_owned)).take(protocol::DEVICES_LIMIT).collect()
    };
    let text = |key: &str| json[key].as_str().unwrap_or_default().to_owned();
    Some(Devices {
        inputs: names("inputs"),
        outputs: names("outputs"),
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
                .child(button("modal-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                })))
                .child(action),
        );
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                })),
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
        let toast = self.copied.filter(|at| at.elapsed() < Duration::from_secs(4)).map(|_| {
            div().absolute().bottom(px(16.)).left_0().right_0().flex().justify_center().child(
                super::components::toast(Some(("icons/check.svg", theme::text_soft())), "Copied").w(px(200.)),
            )
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
            .on_action(cx.listener(|_, _: &NextControl, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &PreviousControl, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &super::CloseWindow, window, cx| {
                this.stop_test(cx);
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
        let root = section_keys!(root, Place1 => 0, Place2 => 1, Place3 => 2, Place4 => 3, Place5 => 4, Place6 => 5, Place7 => 6, Place8 => 7, Place9 => 8);
        root.child(self.list(window, cx))
            .child(self.pane(window, cx))
            .children(menu)
            .children(toast)
            .children(modal)
    }
}

/// `--settings <section> --settings-shot out.png`, with `--shot`: opens Settings on a section
/// once the shell has its first answer and writes that window's frame. `--settings-search
/// <words>` types into the search; `--settings-confirm sign-out|clear|reset|retention` opens
/// that question.
#[cfg(feature = "gui-shot")]
pub fn shot(cx: &mut App) {
    let flag = |name: &str| std::env::args().skip_while(|arg| arg != name).nth(1);
    let (Some(section), Some(path)) = (flag("--settings"), flag("--settings-shot")) else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
        let open_on = Section::ALL.into_iter().find(|s| s.name().to_lowercase().starts_with(&section.to_lowercase())).unwrap_or(Section::Accounts);
        cx.update(|cx| open(open_on, cx));
        let Some(window) = cx.update(|cx| cx.default_global::<Host>().window) else {
            return;
        };
        let _ = window.update(cx, |settings, _, cx| {
            if let Some(words) = flag("--settings-search") {
                settings.query = words.clone();
                settings.search.update(cx, |input, cx| input.set_text(&words, cx));
            }
            settings.confirm = match flag("--settings-confirm").as_deref() {
                Some("sign-out") => settings.shell.upgrade().and_then(|shell| shell.read(cx).snapshot.as_ref()?.accounts.first().cloned()).map(Confirm::SignOut),
                Some("clear") => Some(Confirm::Clear),
                Some("reset") => Some(Confirm::Reset),
                Some("retention") => Some(Confirm::Retention { days: 7, prunable: Some((41_210, 12 * 1024 * 1024)) }),
                _ => None,
            };
            if flag("--settings-menu").as_deref() == Some("input") {
                settings.picker = Some(Picker::Input);
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
            _ => eprintln!("tinyclient: no Settings shot"),
        }
    })
    .detach();
}
