//! A conversation in its own window (#756): the messages and, when this window owns the draft,
//! the composer. No rail and no sidebar. A thread opened from here stays in this window.
//!
//! `Watching` names one conversation and one focused flag. The focused window's conversation
//! is the one sent. A second conversation that is only visible still notifies.
//! shortcut: one watched conversation, until Watching can name every window.

use super::side::thread_key;
use super::tokens::Metrics;
use super::{Pane, PaneEvent};
use crate::client::ask;
use crate::gui::app::prefs;
use crate::gui::components::{glyph, icon_button, tip};
use crate::gui::composer::Composer;
use crate::gui::layout::{self, drags_window, RIGHT_WIDTH, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, Context, Entity, Subscription, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions,
};
use protocol::{Conversation, ConversationKind, Key, Message, Network, Request, Response, Special, ThreadSummary};
use std::rc::Rc;

/// A conversation window is a normal window, large enough to read.
const DEFAULT_SIZE: (f32, f32) = (720., 640.);
const EDGE_MARGIN: f32 = 24.;
const NUDGE: f32 = 28.;

type Frame = (f32, f32, f32, f32);

/// The user asked for a window, and the conversation is one they can already open.
pub(in crate::gui) fn may_pop_out(known: bool, user_opened: bool) -> bool {
    known && user_opened
}

/// Sealed plaintext is drawn only in a window the user opened. Another window does not receive it.
pub(in crate::gui) fn shows_sealed_plaintext(sealed: bool, user_opened_this_window: bool) -> bool {
    !sealed || user_opened_this_window
}

/// A double-click with a modifier opens the window. A plain double-click does not.
pub(in crate::gui) fn modifier_opens_window(click_count: usize, modified: bool) -> bool {
    click_count == 2 && modified
}

pub(in crate::gui) fn modifier_held(modifiers: gpui::Modifiers) -> bool {
    modifiers.shift || modifiers.alt || crate::gui::shortcuts::primary(modifiers)
}

pub(in crate::gui) fn modifier_click(click_count: usize, modifiers: gpui::Modifiers) -> bool {
    modifier_opens_window(click_count, modifier_held(modifiers))
}

/// A modifier press is the pop-out gesture. It does not also open the row,
/// including the first press of the double-click. That press would aim the
/// main window at the conversation, and the new window would have no composer.
pub(in crate::gui) fn press_opens_row(modified: bool) -> bool {
    !modified
}

/// The conversation `Watch` names. The focused pop-out wins; otherwise the main window's,
/// focused or not. An unfocused pop-out is not named.
pub(in crate::gui) fn watched_conversation(main: Option<&Key>, focused_popout: Option<&Key>) -> Option<Key> {
    focused_popout.cloned().or_else(|| main.cloned())
}

/// The pop-out types only when the main window is not already aimed at this conversation.
/// One composer owns the draft.
pub(in crate::gui) fn composer_for_popout(main_key: Option<&Key>, popout_key: &Key) -> bool {
    main_key != Some(popout_key)
}

/// Who is showing a pop-out's draft. `Main` is the open conversation's field.
/// `Side` is the right-pane thread. `Window` is the pop-out itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PopoutDraft {
    Main,
    Side,
    Window,
}

/// A normal window yields only to the open conversation. A thread popped out
/// of the side pane also yields to that side thread, but the open conversation
/// wins when that thread is what the main pane shows. The main field is a
/// different composer from the side-thread slot.
fn popout_draft_home(open: Option<&Key>, side_thread: Option<&Key>, key: &Key, pane_is_thread: bool) -> PopoutDraft {
    if open == Some(key) {
        PopoutDraft::Main
    } else if pane_is_thread && side_thread == Some(key) {
        PopoutDraft::Side
    } else {
        PopoutDraft::Window
    }
}

/// Which slot a thread window copies from when it takes the draft back.
/// The main field wins when it is still on this conversation: that is the
/// text the user just had, and a side composer can still be sitting on an
/// older copy. Otherwise a side composer that is still on the thread. A
/// remembered main draft is the fallback when neither field is on it.
fn thread_draft_from_side(main_aimed: bool, side_aimed: bool, main_remembers: bool) -> bool {
    if main_aimed {
        false
    } else if side_aimed {
        true
    } else {
        !main_remembers
    }
}

/// Restores `saved` when it is finite and at least half on a display. Otherwise the
/// bottom-right of the primary display, nudged so a second window is not on top of the first.
pub(in crate::gui) fn window_frame(saved: Option<Frame>, displays: &[Frame], slot: usize) -> Frame {
    let (min_w, min_h) = layout::WINDOW_MIN;
    if let Some((x, y, w, h)) = saved {
        if x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() {
            let clamped = (x, y, w.max(min_w), h.max(min_h));
            if displays.is_empty() || prefs::frame_visible(clamped, displays) {
                return clamped;
            }
        }
    }
    if let Some(&(dx, dy, dw, dh)) = displays.first() {
        let w = DEFAULT_SIZE.0.min(dw).max(min_w).min(dw.max(min_w));
        let h = DEFAULT_SIZE.1.min(dh).max(min_h).min(dh.max(min_h));
        let nudge = slot as f32 * NUDGE;
        let x = (dx + dw - w - EDGE_MARGIN - nudge).max(dx);
        let y = (dy + dh - h - EDGE_MARGIN - nudge).max(dy);
        return (x, y, w, h);
    }
    (100., 100., DEFAULT_SIZE.0, DEFAULT_SIZE.1)
}

/// Windows the user opened, plus frames still waiting for their conversation to be listed.
#[derive(Default)]
pub(in crate::gui) struct Windows {
    open: Vec<Live>,
    pending: Vec<(Key, Frame)>,
}

struct Live {
    key: Key,
    frame: Frame,
    focused: bool,
    handle: WindowHandle<ConversationWindow>,
    pane: Entity<Pane>,
    thread: Option<Entity<Pane>>,
    composer: Option<Entity<Composer>>,
    thread_composer: Option<Entity<Composer>>,
}

impl Windows {
    pub(in crate::gui) fn set_pending(&mut self, pending: Vec<(Key, Frame)>) {
        self.pending = pending;
    }

    /// Open windows, then any frame whose conversation is not listed yet.
    pub(in crate::gui) fn frames(&self) -> Vec<(Key, Frame)> {
        let mut all: Vec<(Key, Frame)> = self.open.iter().map(|live| (live.key.clone(), live.frame)).collect();
        for (key, frame) in &self.pending {
            if all.iter().all(|(open, _)| open != key) {
                all.push((key.clone(), *frame));
            }
        }
        all
    }

    pub(super) fn panes(&self) -> Vec<Entity<Pane>> {
        self.open
            .iter()
            .flat_map(|live| std::iter::once(live.pane.clone()).chain(live.thread.clone()))
            .collect()
    }

    fn handle(&self, key: &Key) -> Option<WindowHandle<ConversationWindow>> {
        self.open.iter().find(|live| live.key == *key).map(|live| live.handle)
    }

    fn focused_key(&self) -> Option<Key> {
        self.open.iter().find(|live| live.focused).map(|live| live.key.clone())
    }

    fn set_focus(&mut self, key: &Key, focused: bool) {
        for live in &mut self.open {
            if live.key == *key {
                live.focused = focused;
            } else if focused {
                live.focused = false;
            }
        }
    }

    pub(in crate::gui) fn clear_focus(&mut self) {
        for live in &mut self.open {
            live.focused = false;
        }
    }

    fn set_frame(&mut self, key: &Key, frame: Frame) {
        if let Some(live) = self.open.iter_mut().find(|live| live.key == *key) {
            live.frame = frame;
        }
    }

    fn take(&mut self, key: &Key) -> Option<Live> {
        let index = self.open.iter().position(|live| live.key == *key)?;
        Some(self.open.remove(index))
    }
}

struct Prepared {
    key: Key,
    title: String,
    frame: Frame,
    pane: Entity<Pane>,
    composer: Option<Entity<Composer>>,
    /// Put back into prefs if the window does not open.
    restore_pending: Option<Frame>,
}

/// The parent of a thread being opened, and the message it hangs off.
struct ThreadRoot {
    parent: Key,
    root: Rc<Message>,
}

pub struct ConversationWindow {
    shell: WeakEntity<Shell>,
    key: Key,
    title: String,
    pane: Entity<Pane>,
    thread: Option<Entity<Pane>>,
    composer: Option<Entity<Composer>>,
    thread_composer: Option<Entity<Composer>>,
    _keep: Vec<Subscription>,
}

impl ConversationWindow {
    fn new(
        shell: WeakEntity<Shell>,
        key: Key,
        title: String,
        pane: Entity<Pane>,
        composer: Option<Entity<Composer>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_shell = shell.clone();
        let focus_key = key.clone();
        let activation = cx.observe_window_activation(window, move |_, window, cx| {
            let focused = window.is_window_active();
            let _ = focus_shell.update(cx, |shell, cx| shell.note_conversation_focus(&focus_key, focused, cx));
        });
        let bounds_shell = shell.clone();
        let bounds_key = key.clone();
        let bounds = cx.observe_window_bounds(window, move |_, window, cx| {
            let b = window.bounds();
            let (min_w, min_h) = layout::WINDOW_MIN;
            let frame = (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width).max(min_w),
                f32::from(b.size.height).max(min_h),
            );
            let _ = bounds_shell.update(cx, |shell, cx| shell.note_conversation_frame(&bounds_key, frame, cx));
        });
        let thread_shell = shell.clone();
        let thread_key = key.clone();
        let thread = cx.subscribe(&pane, move |_, _, event, cx| {
            let PaneEvent::OpenThread(root) = event else {
                return;
            };
            let (shell, key, root) = (thread_shell.clone(), thread_key.clone(), root.clone());
            // After this update, so the shell can reach this window.
            cx.defer(move |cx| {
                let _ = shell.update(cx, |shell, cx| shell.open_thread_in_window(&key, root, cx));
            });
        });
        Self {
            shell,
            key,
            title,
            pane,
            thread: None,
            composer,
            thread_composer: None,
            _keep: vec![activation, bounds, thread],
        }
    }

    fn note(&self, words: &str) -> gpui::Div {
        div()
            .flex_none()
            .h(px(52.))
            .px(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .border_t_1()
            .border_color(theme::hairline())
            .type_ui()
            .text_color(theme::text_faint())
            .child(words.to_owned())
    }
}

impl Render for ConversationWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let thread_key = self.thread.as_ref().and_then(|pane| pane.read(cx).key().cloned());
        let (title, service, thread_has_draft) = {
            let shell = shell.read(cx);
            let row = shell.conversation_row(&self.key);
            let title = row.as_ref().map(|row| row.title.clone()).unwrap_or_else(|| self.title.clone());
            let service = row.as_ref().is_some_and(|row| row.special == Special::Service);
            let main_thread = shell.main_thread_key(cx);
            let thread_has_draft = thread_key.as_ref().is_some_and(|key| !composer_for_popout(main_thread.as_ref(), key));
            (title, service, thread_has_draft)
        };
        // The field stays until sync has handed the draft off. Hiding it while
        // it is still here would claim the main window holds text it does not.
        let draft = if service {
            let network = self.key.network.label();
            Some(self.note(&format!("Only {network} posts here.")))
        } else if self.composer.is_none() {
            Some(self.note("The draft stays in the main window."))
        } else {
            None
        };
        let thread_note = thread_has_draft.then(|| self.note("The draft stays in the main window."));
        let thread = self.thread.clone().map(|pane| {
            let close = icon_button("popout-thread-close", "icons/x.svg", false)
                .tooltip(tip("Close thread", None))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    let shell = this.shell.clone();
                    let key = this.key.clone();
                    this.thread = None;
                    this.thread_composer = None;
                    let _ = shell.update(cx, |shell, cx| shell.clear_window_thread(&key, cx));
                    cx.notify();
                }));
            div()
                .w(px(RIGHT_WIDTH))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme::hairline())
                .child(
                    div()
                        .h(px(TOP_BAR))
                        .px(px(12.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .child(close)
                        .child(div().flex_1().min_w_0().truncate().type_title().child("Thread")),
                )
                .child(div().flex_1().min_h_0().child(pane))
                .children(thread_note)
                .children(self.thread_composer.clone().filter(|_| !thread_has_draft))
        });
        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(theme::canvas())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        drags_window(
                            div()
                                .h(px(TOP_BAR))
                                .px(px(theme::PANE_PAD))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .border_b_1()
                                .border_color(theme::hairline())
                                .child(glyph("icons/picture-in-picture-2.svg", 14., theme::text_soft()))
                                .child(div().min_w_0().truncate().type_title().child(title)),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(self.pane.clone()))
                    .children(draft)
                    .children(self.composer.clone().filter(|_| !service)),
            )
            .children(thread)
            .into_any_element()
    }
}

fn screens(cx: &App) -> Vec<Frame> {
    cx.displays()
        .iter()
        .map(|display| {
            let bounds = display.bounds();
            (
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            )
        })
        .collect()
}

fn open_prepared(shell: WeakEntity<Shell>, prepared: &Prepared, cx: &mut App) -> Option<WindowHandle<ConversationWindow>> {
    let (x, y, w, h) = prepared.frame;
    let bounds = Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) };
    let (min_w, min_h) = layout::WINDOW_MIN;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(min_w), px(min_h))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(prepared.title.clone().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(20.))),
        }),
        window_decorations: Some(gpui::WindowDecorations::Server),
        focus: true,
        app_id: Some(crate::gui::APP_ID.into()),
        ..Default::default()
    };
    let key = prepared.key.clone();
    let title = prepared.title.clone();
    let pane = prepared.pane.clone();
    let composer = prepared.composer.clone();
    let close_shell = shell.clone();
    let close_key = key.clone();
    match cx.open_window(options, move |window, cx| {
        window.on_window_should_close(cx, move |_, cx| {
            let _ = close_shell.update(cx, |shell, cx| shell.forget_conversation_window(&close_key, cx));
            true
        });
        cx.new(|cx| ConversationWindow::new(shell, key, title, pane, composer, window, cx))
    }) {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!("tinyclient: a conversation window could not open: {err}");
            None
        }
    }
}

impl Shell {
    /// "Open in New Window" and a modified double-click. A phone has one window.
    pub(in crate::gui) fn pop_out_conversation(&mut self, key: Key, cx: &mut Context<Self>) {
        self.schedule_popout(key, None, None, cx);
    }

    /// The main window's right pane is showing a thread.
    pub(in crate::gui) fn thread_is_open(&self) -> bool {
        self.conversation_state.side.stack.thread().is_some()
    }

    /// The thread in the main window's right pane, in a window of its own.
    pub(in crate::gui) fn pop_out_open_thread(&mut self, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            return;
        }
        let Some((parent, root)) = self.conversation_state.side.stack.thread().map(|(key, root)| (key.clone(), root.clone())) else {
            return;
        };
        let kind = self.conversation_row(&parent).map(|row| row.kind).unwrap_or(ConversationKind::Channel);
        let Some(key) = thread_key(&parent, kind, &root) else {
            return;
        };
        self.schedule_popout(key, Some(ThreadRoot { parent, root }), None, cx);
    }

    /// Frames remembered from last launch, once each conversation is in a list.
    pub(in crate::gui) fn restore_conversation_windows(&mut self, cx: &mut Context<Self>) {
        if crate::gui::phone::active() || self.conversation_state.windows.pending.is_empty() {
            return;
        }
        let pending = self.conversation_state.windows.pending.clone();
        let shell = cx.weak_entity();
        cx.defer(move |cx| {
            for (key, frame) in pending {
                let _ = shell.update(cx, |shell, cx| shell.schedule_popout(key, None, Some(frame), cx));
            }
        });
    }

    fn schedule_popout(&mut self, key: Key, thread: Option<ThreadRoot>, saved: Option<Frame>, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            return;
        }
        let shell = cx.weak_entity();
        cx.defer(move |cx| {
            let prepared = shell
                .update(cx, |shell, cx| shell.prepare_popout(key, thread, saved, cx))
                .ok()
                .flatten();
            let Some(prepared) = prepared else {
                return;
            };
            let kept = Prepared {
                key: prepared.key.clone(),
                title: prepared.title.clone(),
                frame: prepared.frame,
                pane: prepared.pane.clone(),
                composer: prepared.composer.clone(),
                restore_pending: prepared.restore_pending,
            };
            match open_prepared(shell.clone(), &prepared, cx) {
                Some(handle) => {
                    let _ = shell.update(cx, |shell, cx| shell.finish_popout(handle, kept, cx));
                }
                None => {
                    let _ = shell.update(cx, |shell, cx| shell.cancel_popout(kept, cx));
                }
            }
        });
    }

    /// A row the user can already open: a list row, or the conversation the main window shows.
    pub(super) fn conversation_row(&self, key: &Key) -> Option<Conversation> {
        self.sidebar_state.find(&self.trees, key).or_else(|| {
            self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).filter(|open| &open.key == key).cloned()
        })
    }

    pub(in crate::gui) fn watched_conversation_key(&self) -> Option<Key> {
        watched_conversation(self.open.as_ref(), self.conversation_state.windows.focused_key().as_ref())
    }

    fn main_thread_key(&self, cx: &App) -> Option<Key> {
        let showing = self.layout_state.right.as_ref().is_some_and(|pane| pane.kind == layout::RightKind::Thread);
        if !showing {
            return None;
        }
        self.conversation_state.thread.borrow().as_ref().and_then(|pane| pane.read(cx).key().cloned())
    }

    fn note_conversation_focus(&mut self, key: &Key, focused: bool, cx: &mut Context<Self>) {
        self.conversation_state.windows.set_focus(key, focused);
        self.sync_watch(cx);
    }

    fn note_conversation_frame(&mut self, key: &Key, frame: Frame, cx: &mut Context<Self>) {
        self.conversation_state.windows.set_frame(key, frame);
        self.save_prefs(cx);
    }

    pub(in crate::gui) fn sync_watch(&mut self, cx: &mut Context<Self>) {
        let pop = self.conversation_state.windows.focused_key();
        let key = watched_conversation(self.open.as_ref(), pop.as_ref());
        self.people_state.hub().watch_conversation(key.as_ref(), cx);
        let focused = self.active || pop.is_some();
        self.people_state.hub().watch_focus(focused, cx);
    }

    /// The main window changed conversation: a pop-out of that same one gives the draft back.
    pub(in crate::gui) fn sync_popout_composers(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<Key> = self.conversation_state.windows.open.iter().map(|live| live.key.clone()).collect();
        for key in keys {
            self.sync_one_composer(&key, cx);
        }
    }

    fn sync_one_composer(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(handle) = self.conversation_state.windows.handle(key) else {
            return;
        };
        // A thread popped out of the side pane is this window's conversation.
        // It yields to the open conversation, whose composer is the main field,
        // and to the right-pane thread when the main pane is somewhere else.
        // A normal window yields only to the open conversation.
        let pane_is_thread = self
            .conversation_state
            .windows
            .open
            .iter()
            .find(|live| live.key == *key)
            .is_some_and(|live| live.pane.read(cx).is_thread());
        let home = popout_draft_home(self.open.as_ref(), self.main_thread_key(cx).as_ref(), key, pane_is_thread);
        let row = self.conversation_row(key);
        // No list row: a thread window still types. A conversation window does not.
        let allowed = row.as_ref().map(|row| row.special != Special::Service).unwrap_or(pane_is_thread);
        let want = allowed && home == PopoutDraft::Window;
        let has = self.conversation_state.windows.open.iter().any(|live| live.key == *key && live.composer.is_some());
        let open_row = row.clone().or_else(|| pane_is_thread.then(|| bare_thread(key)));
        if want && !has {
            if let Some(row) = open_row.clone() {
                let from_side = self.draft_from_side(key, pane_is_thread, cx);
                let composer = self.window_composer(&row, pane_is_thread, from_side, cx);
                if let Some(live) = self.conversation_state.windows.open.iter_mut().find(|live| live.key == *key) {
                    live.composer = Some(composer.clone());
                }
                let _ = handle.update(cx, |view, _, cx| {
                    view.composer = Some(composer);
                    cx.notify();
                });
            }
        } else if !want && has {
            self.drop_window_composer(key, false, cx);
            let _ = handle.update(cx, |view, _, cx| {
                view.composer = None;
                cx.notify();
            });
        } else if want {
            if let Some(row) = open_row {
                if let Some(composer) = self
                    .conversation_state
                    .windows
                    .open
                    .iter()
                    .find(|live| live.key == *key)
                    .and_then(|live| live.composer.clone())
                {
                    self.reaim_window_composer(&composer, &row, pane_is_thread, cx);
                }
            }
        }
        self.sync_window_thread(key, cx);
    }

    /// The thread pane in a conversation window. One composer owns its draft:
    /// this window's while the main window's thread is closed, the main
    /// window's while that thread is open.
    fn sync_window_thread(&mut self, window_key: &Key, cx: &mut Context<Self>) {
        let Some(handle) = self.conversation_state.windows.handle(window_key) else {
            return;
        };
        let thread_key = self
            .conversation_state
            .windows
            .open
            .iter()
            .find(|live| live.key == *window_key)
            .and_then(|live| live.thread.as_ref())
            .and_then(|pane| pane.read(cx).key().cloned());
        let Some(thread_key) = thread_key else {
            return;
        };
        let want = composer_for_popout(self.main_thread_key(cx).as_ref(), &thread_key);
        let has = self.conversation_state.windows.open.iter().any(|live| live.key == *window_key && live.thread_composer.is_some());
        if want && !has {
            let bare = self.thread_row(window_key, thread_key);
            let composer = self.window_composer(&bare, true, true, cx);
            if let Some(live) = self.conversation_state.windows.open.iter_mut().find(|live| live.key == *window_key) {
                live.thread_composer = Some(composer.clone());
            }
            let _ = handle.update(cx, |view, _, cx| {
                view.thread_composer = Some(composer);
                cx.notify();
            });
        } else if !want && has {
            self.drop_window_composer(window_key, true, cx);
            let _ = handle.update(cx, |view, _, cx| {
                view.thread_composer = None;
                cx.notify();
            });
        } else if want {
            let bare = self.thread_row(window_key, thread_key);
            if let Some(composer) = self
                .conversation_state
                .windows
                .open
                .iter()
                .find(|live| live.key == *window_key)
                .and_then(|live| live.thread_composer.clone())
            {
                self.reaim_window_composer(&composer, &bare, true, cx);
            }
        }
    }

    fn thread_row(&self, window_key: &Key, thread_key: Key) -> Conversation {
        let parent = self.conversation_row(window_key);
        let title = parent.as_ref().map(|row| row.title.clone()).unwrap_or_else(|| "Thread".to_owned());
        let mut bare = Conversation::new(thread_key, ConversationKind::Thread, title);
        bare.read_only = parent.is_some_and(|row| row.read_only);
        bare
    }

    /// A listed row changed: the open window picks up its title and what it can do.
    pub(in crate::gui) fn refresh_conversation_window(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(row) = self.conversation_row(key) else {
            return;
        };
        let Some(live) = self.conversation_state.windows.open.iter().find(|live| live.key == *key) else {
            return;
        };
        let (pane, typing) = (live.pane.clone(), live.composer.is_some());
        let me = self.me(key);
        let can = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&key.account())).map(|status| status.can).unwrap_or_default();
        let picture = self.conversation_picture(&row, cx);
        pane.update(cx, |pane, cx| pane.show(&row, &me, can, picture, typing, cx));
        self.sync_one_composer(key, cx);
    }

    fn prepare_popout(&mut self, key: Key, thread: Option<ThreadRoot>, saved: Option<Frame>, cx: &mut Context<Self>) -> Option<Prepared> {
        if crate::gui::phone::active() {
            return None;
        }
        if let Some(handle) = self.conversation_state.windows.handle(&key) {
            if handle.update(cx, |_, window, _| window.activate()).is_ok() {
                self.conversation_state.windows.pending.retain(|(pending, _)| pending != &key);
                return None;
            }
            self.forget_conversation_window(&key, cx);
        }
        let row = self.conversation_row(&key);
        let known = row.is_some() || thread.is_some();
        // Restoring is a window the user opened before. A click is too.
        if !may_pop_out(known, true) {
            return None;
        }
        let parent_sealed = thread.as_ref().and_then(|thread| self.conversation_row(&thread.parent)).is_some_and(|row| row.encryption.is_sealed());
        let sealed = row.as_ref().is_some_and(|row| row.encryption.is_sealed()) || parent_sealed;
        if !shows_sealed_plaintext(sealed, true) {
            return None;
        }
        let as_thread = thread.is_some();
        let home = popout_draft_home(self.open.as_ref(), self.main_thread_key(cx).as_ref(), &key, as_thread);
        let pending_at = self.conversation_state.windows.pending.iter().position(|(pending, _)| pending == &key);
        let restore_pending = pending_at.map(|index| self.conversation_state.windows.pending.remove(index).1);
        let saved = saved.or(restore_pending);
        let frame = window_frame(saved, &screens(cx), self.conversation_state.windows.open.len());
        let title = row.as_ref().map(|row| row.title.clone()).unwrap_or_else(|| "Thread".to_owned());
        let shell = cx.weak_entity();
        let owns = row.as_ref().map(|row| row.special != Special::Service).unwrap_or(true) && home == PopoutDraft::Window;
        let pane = if let Some(thread) = thread {
            let parent = self.conversation_row(&thread.parent);
            let can = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&key.account())).map(|status| status.can).unwrap_or_default();
            let rights = parent.as_ref().map(|row| row.rights).unwrap_or_default();
            let me = self.me(&thread.parent);
            let title = title.clone();
            let root = thread.root.clone();
            let thread_key = key.clone();
            cx.new(move |cx| {
                let mut pane = Pane::new(Metrics::DENSE, true, shell, cx);
                pane.show_thread(thread_key, &title, &me, can, rights, root, cx);
                pane
            })
        } else {
            let row = row.clone()?;
            let me = self.me(&key);
            let can = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&key.account())).map(|status| status.can).unwrap_or_default();
            let picture = self.conversation_picture(&row, cx);
            cx.new(move |cx| {
                let mut pane = Pane::new(Metrics::FULL, false, shell, cx);
                pane.show(&row, &me, can, picture, owns, cx);
                pane
            })
        };
        let from_side = self.draft_from_side(&key, as_thread, cx);
        let composer = if owns {
            if let Some(row) = row.as_ref() {
                Some(self.window_composer(row, as_thread, from_side, cx))
            } else {
                let bare = Conversation::new(key.clone(), ConversationKind::Thread, title.clone());
                Some(self.window_composer(&bare, true, from_side, cx))
            }
        } else {
            None
        };
        Some(Prepared { key, title, frame, pane, composer, restore_pending: saved })
    }

    fn finish_popout(&mut self, handle: WindowHandle<ConversationWindow>, prepared: Prepared, cx: &mut Context<Self>) {
        self.conversation_state.windows.open.push(Live {
            key: prepared.key.clone(),
            frame: prepared.frame,
            focused: true,
            handle,
            pane: prepared.pane,
            thread: None,
            composer: prepared.composer,
            thread_composer: None,
        });
        self.conversation_state.windows.set_focus(&prepared.key, true);
        self.sync_watch(cx);
        self.save_prefs(cx);
        cx.notify();
    }

    fn cancel_popout(&mut self, prepared: Prepared, cx: &mut Context<Self>) {
        if let Some(frame) = prepared.restore_pending {
            self.conversation_state.windows.pending.push((prepared.key.clone(), frame));
        }
        // The window never opened, so the draft goes back to the slot it was copied from.
        let to_side = self.draft_from_side(&prepared.key, prepared.pane.read(cx).is_thread(), cx);
        self.drop_one_composer(prepared.composer, to_side, cx);
        cx.notify();
    }

    fn forget_conversation_window(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(live) = self.conversation_state.windows.take(key) else {
            return;
        };
        let pane_is_thread = live.pane.read(cx).is_thread();
        // Closing while the main pane shows this thread hands the field there.
        // Anything else a thread window held still parks on the side slot.
        let to_side = pane_is_thread
            && popout_draft_home(self.open.as_ref(), self.main_thread_key(cx).as_ref(), key, true) != PopoutDraft::Main;
        self.release_window_composer(live.composer, to_side, cx);
        self.release_window_composer(live.thread_composer, true, cx);
        self.sync_watch(cx);
        self.save_prefs(cx);
        cx.notify();
    }

    fn open_thread_in_window(&mut self, window_key: &Key, root: Rc<Message>, cx: &mut Context<Self>) {
        if self.conversation_state.windows.handle(window_key).is_none() {
            return;
        }
        let kind = self.conversation_row(window_key).map(|row| row.kind).unwrap_or(ConversationKind::Channel);
        if let Some(key) = thread_key(window_key, kind, &root) {
            self.mount_window_thread(window_key, key, root, cx);
            return;
        }
        let can = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&window_key.account())).map(|status| status.can).unwrap_or_default();
        if window_key.network != Network::Discord || !super::actions::starts_thread(&root, &can, kind) {
            return;
        }
        let request = Request::StartThread { conversation: window_key.clone(), message: root.id.clone() };
        let window_key = window_key.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |shell, cx| {
                if shell.conversation_state.windows.handle(&window_key).is_none() {
                    return;
                }
                let id = match answer {
                    Ok(Response::Conversation { conversation }) => conversation.key.id,
                    Ok(Response::Error { detail, .. }) => {
                        shell.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                        return;
                    }
                    _ => return,
                };
                let mut message = (*root).clone();
                message.thread = Some(ThreadSummary { id, replies: 0, last: 0, participants: Vec::new(), unread: 0 });
                shell.open_thread_in_window(&window_key, Rc::new(message), cx);
            });
        })
        .detach();
    }

    fn mount_window_thread(&mut self, window_key: &Key, key: Key, root: Rc<Message>, cx: &mut Context<Self>) {
        let Some(handle) = self.conversation_state.windows.handle(window_key) else {
            return;
        };
        let held = self
            .conversation_state
            .windows
            .open
            .iter()
            .find(|live| live.key == *window_key)
            .and_then(|live| live.thread.as_ref())
            .and_then(|pane| pane.read(cx).key().cloned());
        if held.as_ref() == Some(&key) {
            return;
        }
        let parent = self.conversation_row(window_key);
        let title = parent.as_ref().map(|row| row.title.clone()).unwrap_or_else(|| "Thread".to_owned());
        let rights = parent.as_ref().map(|row| row.rights).unwrap_or_default();
        let can = self.snapshot.as_ref().and_then(|snapshot| snapshot.account(&window_key.account())).map(|status| status.can).unwrap_or_default();
        let me = self.me(window_key);
        let shell = cx.weak_entity();
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(Metrics::DENSE, true, shell, cx);
            pane.show_thread(key.clone(), &title, &me, can, rights, root, cx);
            pane
        });
        let previous = self
            .conversation_state
            .windows
            .open
            .iter_mut()
            .find(|live| live.key == *window_key)
            .and_then(|live| live.thread_composer.take());
        self.drop_one_composer(previous, true, cx);
        if let Some(live) = self.conversation_state.windows.open.iter_mut().find(|live| live.key == *window_key) {
            live.thread = Some(pane.clone());
            live.thread_composer = None;
        }
        let _ = handle.update(cx, |view, _, cx| {
            view.thread = Some(pane);
            view.thread_composer = None;
            cx.notify();
        });
        // The main thread may already own this draft, or it may take it later.
        // Sync decides, and decides again when the main thread opens or closes.
        self.sync_window_thread(window_key, cx);
    }

    fn clear_window_thread(&mut self, window_key: &Key, cx: &mut Context<Self>) {
        let Some(live) = self.conversation_state.windows.open.iter_mut().find(|live| live.key == *window_key) else {
            return;
        };
        live.thread = None;
        let composer = live.thread_composer.take();
        drop(live);
        self.drop_one_composer(composer, true, cx);
        cx.notify();
    }

    fn drop_window_composer(&mut self, key: &Key, nested: bool, cx: &mut Context<Self>) {
        // A thread inside a conversation window always returns to the side slot.
        // The window's own composer returns there only when the right pane is
        // the one taking a popped-out thread. The open conversation's field is
        // the main composer, even when that conversation is the thread.
        let to_side = if nested { true } else { self.primary_returns_to_side(key, cx) };
        let Some(live) = self.conversation_state.windows.open.iter_mut().find(|live| live.key == *key) else {
            return;
        };
        let composer = if nested { live.thread_composer.take() } else { live.composer.take() };
        drop(live);
        self.drop_one_composer(composer, to_side, cx);
    }

    /// The right-pane slot, when this popped-out thread is not the open conversation.
    fn primary_returns_to_side(&self, key: &Key, cx: &App) -> bool {
        let pane_is_thread = self
            .conversation_state
            .windows
            .open
            .iter()
            .find(|live| live.key == *key)
            .is_some_and(|live| live.pane.read(cx).is_thread());
        pane_is_thread && popout_draft_home(self.open.as_ref(), self.main_thread_key(cx).as_ref(), key, true) != PopoutDraft::Main
    }

    /// A thread window copies the side slot when that field is the one still
    /// holding the thread. A normal window always copies the main composer.
    fn draft_from_side(&self, key: &Key, pane_is_thread: bool, cx: &App) -> bool {
        if !pane_is_thread {
            return false;
        }
        let (main_aimed, side_aimed, main_remembers) = self.draft_slot_flags(key, cx);
        thread_draft_from_side(main_aimed, side_aimed, main_remembers)
    }

    fn drop_one_composer(&mut self, composer: Option<Entity<Composer>>, to_side: bool, cx: &mut Context<Self>) {
        self.release_window_composer(composer, to_side, cx);
    }
}

/// A thread that is not a list row. The window still types in it.
fn bare_thread(key: &Key) -> Conversation {
    Conversation::new(key.clone(), ConversationKind::Thread, "Thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> Key {
        Key { network: Network::Tiny, account: "a".into(), id: id.into() }
    }

    #[test]
    fn a_window_opens_only_for_a_conversation_the_user_already_has() {
        assert!(may_pop_out(true, true));
        assert!(!may_pop_out(false, true), "an unknown key is not fetched into a new window");
        assert!(!may_pop_out(true, false), "a window the user did not open does not appear");
    }

    #[test]
    fn a_sealed_chat_stays_sealed_in_a_window_the_user_did_not_open() {
        assert!(shows_sealed_plaintext(true, true));
        assert!(!shows_sealed_plaintext(true, false));
        assert!(shows_sealed_plaintext(false, false));
        assert!(shows_sealed_plaintext(false, true));
    }

    #[test]
    fn a_modifier_double_click_opens_the_window() {
        assert!(modifier_opens_window(2, true));
        assert!(!modifier_opens_window(1, true));
        assert!(!modifier_opens_window(2, false));
        assert!(!modifier_opens_window(3, true));
    }

    #[test]
    fn the_first_press_of_a_modifier_double_click_does_not_open_the_row() {
        assert!(!press_opens_row(true));
        assert!(press_opens_row(false));
        assert!(modifier_opens_window(2, true));
        assert!(!modifier_opens_window(1, true));
    }

    #[test]
    fn the_focused_window_is_the_one_watch_names() {
        let main = key("main");
        let other = key("other");
        assert_eq!(watched_conversation(Some(&main), Some(&other)).as_ref(), Some(&other));
        assert_eq!(watched_conversation(Some(&main), None).as_ref(), Some(&main));
        assert_eq!(watched_conversation(None, Some(&other)).as_ref(), Some(&other));
        assert_eq!(watched_conversation(None, None), None);
    }

    #[test]
    fn the_popout_composer_yields_when_the_main_window_has_the_draft() {
        let main = key("a");
        let other = key("b");
        assert!(!composer_for_popout(Some(&main), &main));
        assert!(composer_for_popout(Some(&main), &other));
        assert!(composer_for_popout(None, &other));
    }

    #[test]
    fn a_popped_out_thread_gives_its_draft_to_the_open_conversation() {
        let thread = key("thread");
        let parent = key("parent");
        let other = key("other");
        assert_eq!(popout_draft_home(Some(&thread), None, &thread, true), PopoutDraft::Main);
        assert_eq!(popout_draft_home(Some(&thread), Some(&thread), &thread, true), PopoutDraft::Main);
        assert_eq!(popout_draft_home(Some(&parent), Some(&thread), &thread, true), PopoutDraft::Side);
        assert_eq!(popout_draft_home(Some(&parent), None, &thread, true), PopoutDraft::Window);
        assert_eq!(popout_draft_home(Some(&other), Some(&other), &thread, true), PopoutDraft::Window);
        assert_eq!(popout_draft_home(Some(&parent), Some(&parent), &parent, false), PopoutDraft::Main);
        assert_eq!(popout_draft_home(Some(&other), Some(&parent), &parent, false), PopoutDraft::Window);
    }

    #[test]
    fn a_thread_window_takes_the_draft_back_from_the_main_field() {
        assert!(!thread_draft_from_side(true, true, false), "the main field is what was just on screen");
        assert!(thread_draft_from_side(false, true, true), "the right pane still has the thread");
        assert!(!thread_draft_from_side(false, false, true));
        assert!(thread_draft_from_side(false, false, false));
    }

    #[test]
    fn a_frame_off_every_display_moves_onto_the_primary_one() {
        let displays = [(0., 0., 1920., 1080.)];
        let kept = window_frame(Some((100., 80., 720., 640.)), &displays, 0);
        assert_eq!(kept, (100., 80., 720., 640.));
        let away = window_frame(Some((-4000., -4000., 720., 640.)), &displays, 0);
        assert!(prefs::frame_visible(away, &displays));
        assert_eq!(away, (1176., 416., 720., 640.));
        let nudged = window_frame(None, &displays, 1);
        assert_eq!(nudged.0, away.0 - NUDGE);
        let tiny = window_frame(Some((10., 10., 10., 10.)), &displays, 0);
        assert!(tiny.2 >= layout::WINDOW_MIN.0 && tiny.3 >= layout::WINDOW_MIN.1);
    }
}
