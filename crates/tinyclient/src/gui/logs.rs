//! The Logs window (Help › Logs, ⇧⌘L, or View logs in Settings › About): the live tail of
//! tinyclient's, chatd's and mediad's files (`log_tail`), with a filter, so a problem can be
//! watched as it happens rather than saved and opened afterwards.
//!
//! The files are read off the main thread every half second while the window is open, and not
//! at all once it closes. A file watcher would wake only on a write, but it is a new dependency
//! for one window that is closed nearly all the time.
//!
//! Verbose logging is chatd's stored `log_level` setting, so all three processes change at once
//! without a restart: chatd moves its own ceiling when the setting is stored and tells a running
//! mediad (`MediaRequest::LogLevel`); the window takes it up with the settings it reads back; and
//! a mediad started later is given it in its environment (`share_session`).

use super::app::prefs;
use super::components::{button, search_field, segment, segmented, ButtonKind};
use super::input::{InputEvent, InputKey, TextInput};
use super::settings::controls::switch;
use super::theme::{self, Type};
use crate::log_tail::{self, Filter, Follow, Level, Line, Source, Tail};
use gpui::{
    div, point, prelude::*, px, size, uniform_list, AnyElement, App, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    Global, KeyBinding, ListHorizontalSizingBehavior, SharedString, Subscription, Task, TitlebarOptions, UniformListScrollHandle, Window,
    WindowBounds, WindowHandle, WindowOptions,
};
use protocol::log_policy::LogPolicy;
use protocol::settings::{GLOBAL, LOG_LEVEL};
use protocol::{Request, Response};
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SIZE: (f32, f32) = (1000., 640.);
const MIN_SIZE: (f32, f32) = (720., 360.);
/// How often the files are read while the window is open.
const POLL: Duration = Duration::from_millis(500);
/// One line of the list.
const ROW: f32 = 18.;
/// The column naming a line's process, when every process is shown.
const SOURCE_COLUMN: f32 = 84.;
/// How long a note ("Copied 40 lines") stays.
const NOTE: Duration = Duration::from_secs(4);

gpui::actions!(logs, [Dismiss]);

/// The window, once it is open.
#[derive(Default)]
struct Host {
    window: Option<WindowHandle<LogsWindow>>,
}

impl Global for Host {}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Logs"))]);
}

/// Opens the Logs window, or brings the open one forward. A phone has no log files to show.
/// Safe to call from inside another view's update.
pub fn open(cx: &mut App) {
    if super::phone::active() {
        return;
    }
    cx.defer(open_now);
}

fn open_now(cx: &mut App) {
    if let Some(window) = cx.default_global::<Host>().window {
        if window.update(cx, |_, window, _| window.activate()).is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(SIZE.0), px(SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: Some(TitlebarOptions {
            title: Some("Logs".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(20.))),
        }),
        window_decorations: Some(gpui::WindowDecorations::Server),
        app_id: Some(super::APP_ID.into()),
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| cx.new(|cx| LogsWindow::new(window, cx))) {
        Ok(window) => cx.default_global::<Host>().window = Some(window),
        Err(err) => tracing::warn!("tinyclient: the Logs window could not open: {err}"),
    }
}

/// Writes each process's debug ring to `logs/bundles` (Save debug log), and returns the files
/// written. Blocks on chatd: call it off the main thread.
pub fn save_bundles() -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(guard) = protocol::log_policy::current() {
        if let Ok(Some(path)) = guard.export_bundle() {
            paths.push(path.display().to_string());
        }
    }
    if let Ok(Response::LogsSaved { paths: more }) = crate::client::ask(&Request::SaveLogs) {
        paths.extend(more);
    }
    paths
}

/// What Save debug log says once it is done.
pub fn saved_note(paths: &[String]) -> String {
    if paths.is_empty() {
        "Debug logs are turned off.".to_owned()
    } else {
        protocol::log_policy::logs_dir().join("bundles").display().to_string()
    }
}

/// Whether this machine's files carry debug lines: the stored Verbose logging, or
/// `TINYCLIENT_LOG_LEVEL` when nothing is stored.
pub fn verbose() -> bool {
    LogPolicy::for_settings(&prefs::values()).verbose()
}

/// The `log_level` to store for Verbose logging `on`. Off clears the row, unless the environment
/// alone would keep the files at debug, when it stores `info` so the switch does what it says.
pub fn verbose_value(on: bool) -> Option<String> {
    if on {
        Some("debug".to_owned())
    } else {
        LogPolicy::from_env().verbose().then(|| "info".to_owned())
    }
}

/// The line under Verbose logging, wherever it is drawn.
pub const VERBOSE_NOTE: &str = "tinyclient, chatd and calls write debug lines at once, and after a restart.";

/// Reads what each file gained. A file that cannot be read this time is tried again next time.
fn read_all(follows: &Mutex<Vec<(Source, Follow)>>) -> Vec<(Source, Vec<String>)> {
    let mut follows = follows.lock().unwrap_or_else(|err| err.into_inner());
    follows.iter_mut().filter_map(|(source, follow)| follow.read().ok().filter(|lines| !lines.is_empty()).map(|lines| (*source, lines))).collect()
}

pub struct LogsWindow {
    tail: Tail,
    filter: Filter,
    search: Entity<TextInput>,
    /// What the list draws: the filter over the tail, as of the last refresh. Held still while
    /// paused, so the line under the pointer does not move.
    visible: Vec<Arc<Line>>,
    /// The longest visible line, which sets how far the list scrolls sideways.
    longest: usize,
    /// The newest line's number when auto-scroll was paused; `None` while it follows the end.
    paused_at: Option<u64>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    note: Option<(Instant, String)>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl LogsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> LogsWindow {
        let search = cx.new(|cx| TextInput::new("Filter lines", cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Changed = event {
                    this.filter.text = input.read(cx).text().to_owned();
                    this.refresh(cx);
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event: &InputKey, window, cx| {
                if *event == InputKey::Escape {
                    this.dismiss(window, cx);
                }
            }),
        ];
        // Where each file has been read to lives with the loop, which is dropped with the window.
        let reading = Arc::new(Mutex::new(Source::ALL.iter().map(|source| (*source, Follow::new(source.path()))).collect::<Vec<_>>()));
        let poll = cx.spawn(async move |this, cx| loop {
            let follows = reading.clone();
            let read = cx.background_executor().spawn(async move { read_all(&follows) }).await;
            if this.update(cx, |this, cx| this.take(read, cx)).is_err() {
                break;
            }
            cx.background_executor().timer(POLL).await;
        });
        let focus = cx.focus_handle();
        window.focus(&search.focus_handle(cx), cx);
        LogsWindow {
            tail: Tail::default(),
            filter: Filter::default(),
            search,
            visible: Vec::new(),
            longest: 0,
            paused_at: None,
            scroll: UniformListScrollHandle::new(),
            focus,
            note: None,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    /// Lines read from the files. Shown at once unless paused; counted on Resume if they are.
    fn take(&mut self, read: Vec<(Source, Vec<String>)>, cx: &mut Context<Self>) {
        if read.is_empty() {
            return;
        }
        for (source, lines) in read {
            self.tail.push(source, lines);
        }
        if self.paused_at.is_none() {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    /// Draws the filter over the tail again, and follows the end unless paused.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.visible = self.tail.visible(&self.filter);
        self.longest = self.visible.iter().enumerate().max_by_key(|(_, line)| line.text.len()).map_or(0, |(ix, _)| ix);
        if self.paused_at.is_none() {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        self.paused_at = paused.then(|| self.tail.seq());
        self.refresh(cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter.text.is_empty() {
            self.search.update(cx, |input, cx| input.set_text("", cx));
            self.filter.text.clear();
            self.refresh(cx);
            return;
        }
        window.remove_window();
    }

    fn noted(&mut self, words: String, cx: &mut Context<Self>) {
        self.note = Some((Instant::now(), words));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn copy_visible(&mut self, cx: &mut Context<Self>) {
        let text = log_tail::copy_text(&self.visible, self.filter.source.is_none());
        let count = self.visible.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.noted(format!("Copied {count} {}", if count == 1 { "line" } else { "lines" }), cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let paths = cx.background_executor().spawn(async { save_bundles() }).await;
            let _ = this.update(cx, |this, cx| this.noted(format!("Saved to {}", saved_note(&paths)), cx));
        })
        .detach();
    }

    /// Stores Verbose logging in chatd, which moves its own ceiling and a call's at once; this
    /// process moves when the answer is in.
    fn set_verbose(&mut self, on: bool, cx: &mut Context<Self>) {
        let value = verbose_value(on);
        let request = Request::Set { scope: GLOBAL.to_owned(), key: LOG_LEVEL.to_owned(), value: value.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(answer, Ok(Response::Ok)) {
                    this.noted("Verbose logging could not be changed: chatd is not running.".to_owned(), cx);
                    return;
                }
                let mut values = prefs::values();
                values.set(GLOBAL, LOG_LEVEL, value);
                if let Some(guard) = protocol::log_policy::current() {
                    guard.apply(LogPolicy::for_settings(&values));
                }
                prefs::set_values(values);
                cx.notify();
            });
        })
        .detach();
    }

    fn rows(&mut self, range: Range<usize>, _: &mut Window, _: &mut Context<Self>) -> Vec<AnyElement> {
        let every = self.filter.source.is_none();
        range
            .filter_map(|ix| self.visible.get(ix))
            .map(|line| {
                let color = match line.level {
                    Some(Level::Error) => theme::danger(),
                    Some(Level::Warn) => theme::warning(),
                    Some(Level::Debug | Level::Trace) => theme::text_faint(),
                    Some(Level::Info) | None => theme::text(),
                };
                div()
                    .h(px(ROW))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .font_family(theme::MONO)
                    .text_size(theme::sized(12.))
                    .line_height(px(ROW))
                    .when(every, |row| row.child(div().flex_none().w(px(SOURCE_COLUMN)).text_color(theme::text_faint()).child(line.source.name())))
                    .child(div().flex_none().text_color(color).child(SharedString::from(line.text.clone())))
                    .into_any_element()
            })
            .collect()
    }

    fn toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search.focus_handle(cx).is_focused(window);
        let field = search_field(focused).w(px(240.)).child(div().flex_1().min_w_0().child(self.search.clone()));
        let mut levels = segmented();
        for level in Level::SHOWN {
            levels = levels.child(segment(SharedString::from(format!("level-{}", level.name())), level.name(), self.filter.level == level).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.filter.level = level;
                    this.refresh(cx);
                }),
            ));
        }
        let hide_noise = self.filter.hide_noise;
        let noise = div()
            .id("noise")
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .child(switch("noise-switch", "Hide noise", hide_noise))
            .child(div().type_ui().text_color(theme::text_soft()).child("Hide noise"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.filter.hide_noise = !hide_noise;
                this.refresh(cx);
            }));
        let pause = match self.paused_at {
            None => button("pause", ButtonKind::Secondary, "Pause").on_click(cx.listener(|this, _, _, cx| this.set_paused(true, cx))),
            Some(at) => {
                let waiting = self.tail.seq().saturating_sub(at);
                let label = if waiting == 0 { "Resume".to_owned() } else { format!("Resume ({waiting} new)") };
                button("pause", ButtonKind::Primary, label).on_click(cx.listener(|this, _, _, cx| this.set_paused(false, cx)))
            }
        };
        div()
            .flex_none()
            .h(px(44.))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(field)
            .child(levels)
            .child(noise)
            .child(div().flex_1())
            .child(pause)
            .child(button("copy", ButtonKind::Secondary, "Copy visible").on_click(cx.listener(|this, _, _, cx| this.copy_visible(cx))))
            .child(button("save", ButtonKind::Secondary, "Save debug log").on_click(cx.listener(|this, _, _, cx| this.save(cx))))
    }

    fn strip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sources = segmented().child(segment("source-all", "All", self.filter.source.is_none()).on_click(cx.listener(|this, _, _, cx| {
            this.filter.source = None;
            this.refresh(cx);
        })));
        for source in Source::ALL {
            sources = sources.child(segment(SharedString::from(format!("source-{}", source.name())), source.name(), self.filter.source == Some(source)).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.filter.source = Some(source);
                    this.refresh(cx);
                }),
            ));
        }
        // The traffic lights sit at the left on macOS; Windows' buttons are drawn at the right.
        let left = if cfg!(target_os = "macos") { 84. } else { 16. };
        super::layout::drags_window(div())
            .h(px(theme::STRIP))
            .flex_none()
            .pl(px(left))
            .pr(px(super::chrome::width(window) + 12.))
            .flex()
            .items_center()
            .gap(px(16.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(div().type_title().text_color(theme::text()).child("Logs"))
            .child(sources)
    }

    fn footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = verbose();
        let shown = self.visible.len();
        let kept: usize = Source::ALL.iter().map(|source| self.tail.len(*source)).sum();
        div()
            .flex_none()
            .h(px(40.))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_t_1()
            .border_color(theme::hairline())
            .bg(theme::sidebar())
            .child(
                div()
                    .id("verbose")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .child(switch("verbose-switch", "Verbose logging", on))
                    .child(div().type_ui().text_color(theme::text()).child("Verbose logging"))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_verbose(!on, cx))),
            )
            .child(div().type_small().text_color(theme::text_faint()).child(VERBOSE_NOTE))
            .child(div().flex_1())
            .child(div().type_small().text_color(theme::text_faint()).child(format!("{shown} of {kept} lines")))
            .child(button("reveal", ButtonKind::Ghost, crate::host::REVEAL).on_click(cx.listener(|_, _, _, cx| {
                cx.reveal_path(&protocol::log_policy::logs_dir());
            })))
    }
}

impl Focusable for LogsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LogsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.visible.is_empty() {
            let words = if self.tail.seq() == 0 { "No lines yet. They appear here as they are written." } else { "No lines match the filter." };
            div().flex_1().flex().items_center().justify_center().type_ui().text_color(theme::text_faint()).child(words).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .py(px(4.))
                .child(
                    uniform_list("log-lines", self.visible.len(), cx.processor(Self::rows))
                        .with_width_from_item(Some(self.longest))
                        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                        .track_scroll(&self.scroll)
                        .size_full(),
                )
                .into_any_element()
        };
        let note = self.note.as_ref().filter(|(at, _)| at.elapsed() < NOTE).map(|(_, words)| {
            div().absolute().bottom(px(56.)).left_0().right_0().flex().justify_center().child(
                super::components::toast(Some(("icons/check.svg", theme::text_soft())), words.clone()).w(px(420.)),
            )
        });
        let close = super::chrome::buttons(window, |_, window, _| window.remove_window());
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .key_context("Logs")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|_, _: &super::CloseWindow, window, _| window.remove_window()))
            .child(self.strip(window, cx))
            .child(self.toolbar(window, cx))
            .child(body)
            .child(self.footer(cx))
            .children(note)
            .children(close)
            .children(super::chrome::edges(window))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_on_stores_debug_and_off_clears_the_row() {
        assert_eq!(verbose_value(true).as_deref(), Some("debug"));
        // The test environment sets no TINYCLIENT_LOG_LEVEL.
        if std::env::var("TINYCLIENT_LOG_LEVEL").is_err() {
            assert_eq!(verbose_value(false), None);
        }
        assert!(protocol::settings::valid(GLOBAL, LOG_LEVEL, "debug") && protocol::settings::valid(GLOBAL, LOG_LEVEL, "info"));
    }
}
