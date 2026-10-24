//! The window as an application: keeping chatd running and saying what the window waits on
//! (`service`), crash protection (`crash`), what the window remembers (`prefs`) and the menu bar
//! (`menubar`). `polish/01-system.md` §5–7.
//!
//! For the other views:
//! - `Shell::window_banner(cx)`: the one window-level banner, when there is one. The sidebar's
//!   banners should start with it (it outranks every account's).
//! - `Shell::details_switch(direct)` / `set_details_switch(direct, on, cx)`: the two Details
//!   switches, remembered across launches.
//! - `Shell::save_prefs(cx)`: after changing anything `prefs::Prefs` holds.
//! - `app::reduce_motion()`: animate nothing when it is true.

pub mod crash;
pub mod menubar;
pub mod prefs;
pub mod service;

use super::components::{banner, banner_action, button, empty_state_with, glyph, toast, ButtonKind, Tone};
use super::layout::{self, drags_window};
use super::state::Shell;
use super::theme;
use crate::client::{ask, ask_within, is_gone, is_other_version, is_slow};
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, Window};
use protocol::{Request, Response};
use service::{Phase, Shown, Stopped};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How soon to ask again when chatd did not answer.
const RETRY: Duration = Duration::from_secs(2);
/// How often to look for a chatd the shell has just started.
const POLL: Duration = Duration::from_millis(250);
/// A started chatd gets this long to answer before it is started again.
const START_GRACE: Duration = Duration::from_secs(8);
/// A move or resize is saved once it has been still this long.
const FRAME_SETTLE: Duration = Duration::from_millis(500);

static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// macOS's Reduce Motion: menus, popovers, modals and toasts then appear at once.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

/// Reads Reduce Motion once, off the main thread, at launch.
pub fn read_reduce_motion() {
    let _ = std::thread::Builder::new().name("reduce-motion".into()).stack_size(64 * 1024).spawn(|| {
        let answer = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(output) = answer {
            REDUCE_MOTION.store(String::from_utf8_lossy(&output.stdout).trim() == "1", Ordering::Relaxed);
        }
    });
}

/// Diagnostics, with what takes a process to find out: the operating system and the window's
/// memory. Off the main thread.
pub(in crate::gui) fn with_memory(mut text: String) -> String {
    let run = |program: &str, args: &[&str]| command_out(program, args);
    if let Some(version) = os_version() {
        text.push_str(&format!("{version}\n"));
    }
    let pid = std::process::id().to_string();
    if let Some(kb) = run("/bin/ps", &["-o", "rss=", "-p", &pid]).and_then(|kb| kb.parse::<u64>().ok()) {
        text.push_str(&format!("window memory: {} MB\n", kb / 1024));
    }
    text
}

/// A helper's trimmed output. Without a console, so Copy diagnostics flashes no window on Windows.
fn command_out(program: &str, args: &[&str]) -> Option<String> {
    protocol::launch::no_console(std::process::Command::new(program).args(args).stderr(std::process::Stdio::null()))
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|out| !out.is_empty())
}

/// The operating system line in copied diagnostics. macOS from `sw_vers`, Linux from
/// os-release, Windows from `ver`.
#[cfg(target_os = "macos")]
fn os_version() -> Option<String> {
    command_out("/usr/bin/sw_vers", &["-productVersion"]).map(|version| format!("macOS {version}"))
}

#[cfg(target_os = "linux")]
fn os_version() -> Option<String> {
    Some(crate::host::os_release_name())
}

#[cfg(windows)]
fn os_version() -> Option<String> {
    command_out("cmd", &["/C", "ver"])
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn os_version() -> Option<String> {
    None
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Shell {
    /// Keeps the shell connected for as long as the window lives: asks `Status`, starts chatd
    /// when nobody holds its socket, then holds one `Wait` open. chatd holds each `Wait` until
    /// something changes, so an idle shell does not wake.
    pub(in crate::gui) fn watch(cx: &mut Context<Self>) {
        // The Settings window reads the accounts from this shell.
        super::settings::attach(cx.entity().downgrade(), cx);
        cx.spawn(async move |this, cx| loop {
            let status = cx.background_executor().spawn(async { ask_within(&Request::Status, Some(service::SLOW)) }).await;
            match status {
                Ok(Response::Status { networks, accounts, call, seq, keychain, started, .. }) => {
                    let applied = this.update(cx, |this, cx| {
                        let came_back = this.service.phase == Phase::Reconnecting;
                        let restarted = this.service.answered(started, keychain, unix_now());
                        this.apply_status(networks, accounts, call, cx);
                        this.load_settings(cx);
                        this.reload_all(cx);
                        if restarted || came_back {
                            this.conversation_reset(cx);
                        }
                        this.restore_place(cx);
                    });
                    if applied.is_err() {
                        return;
                    }
                    let mut since = seq;
                    loop {
                        let wait = async move { ask(&Request::Wait { since }) };
                        match cx.background_executor().spawn(wait).await {
                            Ok(Response::Changes { seq, reset, changes }) => {
                                since = seq;
                                let applied = this.update(cx, |this, cx| {
                                    if reset {
                                        this.load_status(cx);
                                        this.load_settings(cx);
                                        this.reload_all(cx);
                                        this.conversation_reset(cx);
                                    } else {
                                        this.changed(&changes, cx);
                                    }
                                });
                                if applied.is_err() {
                                    return;
                                }
                            }
                            // A `Wait` that outlived the read timeout: chatd is there, ask again.
                            Err(err) if is_slow(&err) => {}
                            _ => break,
                        }
                    }
                    // The socket went away mid-session: what is on screen stays.
                    if this.update(cx, |this, cx| this.enter_phase(Phase::Reconnecting, cx)).is_err() {
                        return;
                    }
                    // Look again at once: a chatd that is gone is started without waiting.
                    continue;
                }
                Err(err) if is_slow(&err) => {
                    // chatd holds its socket and says nothing yet: it is still starting.
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
                Err(err) if is_gone(&err) => {
                    let Ok(start) = this.update(cx, |this, cx| this.chatd_gone(cx)) else {
                        return;
                    };
                    if start {
                        let started = cx.background_executor().spawn(async { protocol::launch::start_chatd() }).await;
                        let Ok(()) = this.update(cx, |this, cx| match started {
                            Ok(pid) => {
                                this.service.pid = pid;
                                this.service.launched = Some(Instant::now());
                            }
                            Err(_) => this.stop_trying(Stopped::CannotStart, cx),
                        }) else {
                            return;
                        };
                    }
                    // A chatd just started is looked for often until it answers; otherwise the
                    // shell asks again at the usual pace.
                    let Ok(fresh) = this.update(cx, |this, _| this.service.launched.is_some_and(|at| at.elapsed() < START_GRACE)) else {
                        return;
                    };
                    cx.background_executor().timer(if fresh { POLL } else { RETRY }).await;
                    continue;
                }
                // An answer that cannot be read, or the wrong one: another version.
                other => {
                    let mismatch = match &other {
                        Err(err) => is_other_version(err),
                        Ok(_) => true,
                    };
                    // Once per launch: the old chatd is asked to make way, and the matching one
                    // is started in its place by the next pass.
                    let Ok(replace) = this.update(cx, |this, _| {
                        let replace = mismatch && this.service.may_start && !this.service.replaced;
                        this.service.replaced |= replace;
                        replace
                    }) else {
                        return;
                    };
                    let replaced = if replace { cx.background_executor().spawn(async { service::stop_other_version() }).await } else { false };
                    if replaced {
                        continue;
                    }
                    if this.update(cx, |this, cx| if mismatch { this.stop_trying(Stopped::OtherVersion, cx) }).is_err() {
                        return;
                    }
                }
            }
            cx.background_executor().timer(RETRY).await;
        })
        .detach();
    }

    /// Nobody holds chatd's socket. Answers whether to start it now: a chatd started moments ago
    /// gets time to open its socket, and three starts in 30 s are the last.
    fn chatd_gone(&mut self, cx: &mut Context<Self>) -> bool {
        let fresh = self.service.launched.is_some_and(|at| at.elapsed() < START_GRACE);
        if fresh || self.service.phase == Phase::Failed {
            return false;
        }
        if self.snapshot.is_some() {
            self.enter_phase(Phase::Reconnecting, cx);
        }
        if !self.service.may_start {
            return false;
        }
        if protocol::launch::chatd_path().is_none() {
            self.stop_trying(Stopped::CannotStart, cx);
            return false;
        }
        if !self.service.restarts.allow(Instant::now()) {
            self.stop_trying(Stopped::KeepsStopping, cx);
            return false;
        }
        true
    }

    /// The shell stops starting chatd and the window says why, until "Try again".
    fn stop_trying(&mut self, why: Stopped, cx: &mut Context<Self>) {
        self.service.stopped = Some(why);
        self.enter_phase(Phase::Failed, cx);
    }

    /// Moves to a phase and wakes the window once at each threshold it may cross: the skeleton
    /// at 150 ms and the words at 3 s. Nothing ticks in between.
    pub(in crate::gui) fn enter_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if self.service.phase == phase {
            return;
        }
        self.service.enter(phase);
        cx.notify();
        Self::wake_at_thresholds(cx);
    }

    /// One wake at 150 ms and one at 3 s from now, for the states that appear then.
    pub(in crate::gui) fn wake_at_thresholds(cx: &mut Context<Self>) {
        for after in [service::SKELETON, service::SLOW] {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(after).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    /// "Try again" on the failed banner: start counting restarts from nothing.
    fn try_again(&mut self, cx: &mut Context<Self>) {
        self.service.restarts.clear();
        self.service.launched = None;
        self.service.stopped = None;
        self.service.replaced = false;
        let phase = if self.snapshot.is_some() { Phase::Reconnecting } else { Phase::Starting };
        self.enter_phase(phase, cx);
    }

    /// "Try again" on the keychain's refusal: chatd reads the refused sessions again.
    fn read_sessions(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || ask(&Request::ReadSessions), |this, answer, cx| {
            if let Ok(Response::Status { keychain, started, .. }) = answer {
                this.service.answered(started, keychain, unix_now());
                cx.notify();
            }
        });
    }

    /// Once the first answer is in: the conversation and sidebar mode of the last launch.
    fn restore_place(&mut self, cx: &mut Context<Self>) {
        let Some((open, mode)) = self.restore.take() else {
            return;
        };
        // A restored chat would cover the server page. Tiny opens on its own window.
        let tiny = matches!(&mode, super::sidebar::Mode::Network(protocol::Network::Tiny));
        if tiny || mode != self.sidebar_state.mode {
            self.go(mode, cx);
        }
        if tiny {
            return;
        }
        if let Some((key, title)) = open {
            if self.open.is_none() {
                self.open_now(key, &title, cx);
            }
        }
    }

    /// What the window says now (`polish/01-system.md` §5).
    pub(in crate::gui) fn shown(&self) -> Shown {
        #[cfg(feature = "gui-shot")]
        if let Some(forced) = &self.forced_state {
            return forced.clone();
        }
        service::shown(&self.service, Instant::now())
    }

    /// The one window-level banner, outranking every account's: starting, keychain, reconnecting,
    /// not receiving. `None` while chatd answers and the keychain is settled.
    pub(in crate::gui) fn window_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let shown = self.shown();
        let words = shown.banner()?;
        let tone = match shown {
            Shown::Failed => Tone::Danger,
            Shown::KeychainWaiting => Tone::Keychain,
            _ => Tone::Warning,
        };
        let line = banner(tone, words);
        Some(match shown {
            Shown::Failed => {
                let line = line.child(banner_action("service-retry", "Try again").on_click(cx.listener(|this, _, _, cx| this.try_again(cx))));
                match self.service.stopped {
                    Some(why) => div().child(div().id("not-receiving").tooltip(super::components::tip(why.words(), None)).child(line)),
                    None => line,
                }
            }
            Shown::Reconnecting => {
                let saved = self.service.saved_at.map(|at| service::saved_words(&super::time::clock(at)));
                let tip = saved.unwrap_or_default();
                div().child(div().id("reconnecting").tooltip(super::components::tip(tip, None)).child(line))
            }
            _ => line,
        })
    }

    /// The conversation region while the keychain holds the window, when nothing is open.
    pub(in crate::gui) fn keychain_state(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let shown = self.shown();
        let (icon, title, line) = shown.empty_state()?;
        if self.snapshot.as_ref().is_some_and(|s| s.open.is_some()) {
            return None;
        }
        let body = match shown {
            Shown::KeychainDenied(_) => empty_state_with(
                icon,
                title,
                line,
                button("keychain-retry", ButtonKind::Secondary, "Try again")
                    .on_click(cx.listener(|this, _, _, cx| this.read_sessions(cx))),
            ),
            _ => super::components::empty_state(icon, title, line),
        };
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    drags_window(div())
                        .h(px(theme::STRIP))
                        .flex_none()
                        .px(px(theme::PANE_PAD))
                        .flex()
                        .items_center()
                        .child(self.strip_controls(window, cx)),
                )
                .child(div().flex_1().min_h_0().child(body)),
        )
    }

    /// Asks GitHub shortly after launch, then about once an hour, and keeps the notice up
    /// when a newer release exists.
    pub(in crate::gui) fn watch_updates(cx: &mut Context<Self>) {
        if cfg!(feature = "gui-shot") {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            loop {
                let automatic = this.update(cx, |_, _| prefs::choices().updates).unwrap_or(true);
                cx.background_executor()
                    .spawn(async move { crate::update::pursue(crate::update::Policy::Now { automatic }) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if crate::update::announce().is_some() {
                            this.layout_state.update_alert = true;
                        }
                        this.refresh_menus(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                // The settings window is a different entity. Notifying it from inside the shell
                // update above leases the shell twice and aborts.
                let _ = cx.update(|cx| super::settings::redraw(cx));
                cx.background_executor().timer(Duration::from_secs(3600)).await;
            }
        })
        .detach();
    }

    fn update_alert_act(&mut self, cx: &mut Context<Self>) {
        match crate::update::shown() {
            crate::update::Shown::Ready { .. } => cx.quit(),
            crate::update::Shown::Available { page, .. } => {
                if crate::update::install_into().is_some() {
                    self.off_thread(cx, || crate::update::pursue(crate::update::Policy::Install), |this, _, cx| {
                        this.layout_state.update_alert = true;
                        cx.notify();
                    });
                } else {
                    cx.open_url(&page);
                }
            }
            _ => {}
        }
    }

    /// Stays until dismissed. Install downloads; Restart quits so the staged app replaces this one.
    pub(in crate::gui) fn update_toast(&self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.layout_state.update_alert {
            return None;
        }
        let (words, label) = match crate::update::shown() {
            crate::update::Shown::Ready { version } => (format!("Version {version} is available. Restart to install it."), "Restart"),
            crate::update::Shown::Available { version, .. } => {
                let label = if crate::update::install_into().is_some() { "Install" } else { "Download" };
                (format!("Version {version} is available."), label)
            }
            _ => return None,
        };
        let action = button("update-act", ButtonKind::Ghost, label).h(px(24.)).px(px(8.)).text_color(theme::text()).on_click(cx.listener(|this, _, _, cx| {
            this.update_alert_act(cx);
        }));
        let close = div().id("update-close").flex_none().cursor_pointer().child(glyph("icons/x.svg", 14., theme::text_faint())).on_click(
            cx.listener(|this, _, _, cx| {
                this.layout_state.update_alert = false;
                cx.notify();
            }),
        );
        Some(
            toast(Some(("icons/download.svg", theme::accent())), words)
                .w(px(width))
                .child(action)
                .child(close)
                .into_any_element(),
        )
    }

    /// The crash-restart toast: error style, stays until dismissed.
    pub(in crate::gui) fn crash_toast(&self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.crashed?;
        let words = "tinyclient quit unexpectedly and reopened.";
        let copy = button("copy-report", ButtonKind::Ghost, "Copy report").h(px(24.)).px(px(8.)).text_color(theme::text()).on_click(
            cx.listener(|this, _, _, cx| {
                this.crashed = None;
                this.off_thread(cx, crash::read_report, |this, report, cx| {
                    let report = report.unwrap_or_else(|| "tinyclient quit unexpectedly. No report was written.".to_owned());
                    cx.write_to_clipboard(ClipboardItem::new_string(report));
                    this.toast(Some(("icons/check.svg", theme::text_soft())), "Report copied", cx);
                });
                cx.notify();
            }),
        );
        let close = div()
            .id("crash-close")
            .flex_none()
            .cursor_pointer()
            .child(glyph("icons/x.svg", 14., theme::text_faint()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.crashed = None;
                cx.notify();
            }));
        Some(
            toast(Some(("icons/circle-alert.svg", theme::danger())), words)
                .w(px(width))
                .child(copy)
                .child(close)
                .into_any_element(),
        )
    }

    // What the window remembers.

    /// The Details switch for direct conversations (`direct`) or for channels and groups.
    #[allow(dead_code)]
    pub(in crate::gui) fn details_switch(&self, direct: bool) -> bool {
        let state = &self.layout_state;
        if direct { state.details_directs } else { state.details_channels }
    }

    #[allow(dead_code)]
    pub(in crate::gui) fn set_details_switch(&mut self, direct: bool, on: bool, cx: &mut Context<Self>) {
        let state = &mut self.layout_state;
        let switch = if direct { &mut state.details_directs } else { &mut state.details_channels };
        if *switch != on {
            *switch = on;
            self.save_prefs(cx);
        }
    }

    pub(in crate::gui) fn current_prefs(&self) -> prefs::Prefs {
        let state = &self.layout_state;
        let open = self.open.clone().map(|key| {
            let title = self.snapshot.as_ref().and_then(|s| s.open.as_ref()).map(|c| c.title.clone()).unwrap_or_default();
            (key, title)
        });
        prefs::Prefs {
            sidebar: state.sidebar_width,
            right: state.right_width,
            pins: self.sidebar_state.pins.clone(),
            frame: self.frame.get(),
            // Until the first answer, the place of the last launch is still the one to keep.
            open: match &self.restore {
                Some((open, _)) => open.clone(),
                None => open,
            },
            mode: match &self.restore {
                Some((_, mode)) => mode.clone(),
                None => self.sidebar_state.mode.clone(),
            },
            details_channels: state.details_channels,
            details_directs: state.details_directs,
            text_step: theme::text_step(),
            choices: prefs::choices(),
            kept: self.prefs_kept.clone(),
            autoplay: super::anim::autoplay(),
        }
    }

    /// Reads chatd's settings: at start, after a restart, and whenever one changes. Every
    /// window draws again with them.
    pub(in crate::gui) fn load_settings(&mut self, cx: &mut Context<Self>) {
        self.off_thread(cx, || ask(&Request::Settings), |_, answer, cx| {
            if let Ok(Response::Settings { values }) = answer {
                prefs::set_values(protocol::settings::Values::new(values));
                cx.refresh_windows();
            }
        });
    }

    /// Writes what the window remembers, off the main thread. A screenshot run writes nothing.
    pub(in crate::gui) fn save_prefs(&mut self, cx: &mut Context<Self>) {
        let text = prefs::write(&self.current_prefs());
        self.saved_prefs.replace(text.clone());
        if cfg!(feature = "gui-shot") {
            return;
        }
        let path = protocol::support_dir().join(prefs::FILE);
        self.off_thread(cx, move || std::fs::write(path, text), |_, _, _| {});
    }

    /// Called each frame: saves when the open conversation or mode changed since the last save.
    /// A string compare, nothing more, when nothing did.
    pub(in crate::gui) fn note_place(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.restore.is_some() {
            return;
        }
        let origin = window.bounds();
        let frame = (f32::from(origin.origin.x), f32::from(origin.origin.y), f32::from(origin.size.width), f32::from(origin.size.height));
        if !window.is_fullscreen() && !window.is_maximized() {
            self.frame.set(Some(frame));
        }
        let text = prefs::write(&self.current_prefs());
        if *self.saved_prefs.borrow() != text {
            self.saved_prefs.replace(text);
            let seen = self.frame.get();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FRAME_SETTLE).await;
                let _ = this.update(cx, |this, cx| {
                    if this.frame.get() == seen {
                        this.save_prefs(cx);
                    }
                });
            })
            .detach();
        }
    }

    /// ⌘+, ⌘−, ⌘0.
    pub(in crate::gui) fn step_text(&mut self, by: Option<i32>, window: &mut Window, cx: &mut Context<Self>) {
        let step = by.map_or(0, |by| theme::text_step() + by);
        if theme::set_text_step(step) != step && by.is_some() {
            return;
        }
        window.refresh();
        self.save_prefs(cx);
    }

    /// Help › Copy Diagnostics and Settings › About: versions, the connection and each account's
    /// link, in words a support reply can use. Nothing that names a person or a message.
    pub(in crate::gui) fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let text = self.diagnostics();
        self.off_thread(cx, move || with_memory(text), |this, text, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            this.toast(Some(("icons/check.svg", theme::text_soft())), "Diagnostics copied", cx);
        });
    }

    pub(in crate::gui) fn diagnostics(&self) -> String {
        let mut text = format!(
            "tinyclient {}\n{} {}\nservice: {:?}{}, keychain: {:?}, started {}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.service.phase,
            self.service.stopped.map(|why| format!(" ({why:?})")).unwrap_or_default(),
            self.service.keychain,
            self.service.started
        );
        for account in self.snapshot.iter().flat_map(|s| &s.accounts) {
            text.push_str(&format!("{}: {:?}{}\n", account.key.network.label(), account.link, if account.reconnecting { " (reconnecting)" } else { "" }));
        }
        protocol::redact(&text)
    }

    /// The window's banner while the sidebar is hidden. A drawn sidebar puts
    /// [`Shell::window_banner`] first in its own flow, so nothing is drawn over its rows.
    pub(in crate::gui) fn banner_layer(&self, sidebar: Option<f32>, cx: &mut Context<Self>) -> Option<Div> {
        if sidebar.is_some() {
            return None;
        }
        let line = self.window_banner(cx)?;
        Some(
            div()
                .absolute()
                .left(px(layout::RAIL_WIDTH))
                .top(px(theme::STRIP + 8.))
                .w(px(360.))
                .px(px(theme::INSET))
                .pt(px(8.))
                .pb(px(8.))
                .bg(theme::sidebar())
                .occlude()
                .child(line),
        )
    }
}
