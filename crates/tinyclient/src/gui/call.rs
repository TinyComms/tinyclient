//! Calls in the shell (`docs/design/07-calls.md`): the header pill that starts, leaves or hands
//! off a call, and the dock at the bottom of the sidebar that is every state of one. The dock is
//! in `dock`, its device menu in `devices`, who is in the call in `people`.
//!
//! Nothing here talks to chatd on the main thread, and nothing is drawn as "in a call" before
//! chatd says `Joined`.

mod devices;
mod dock;
mod people;
mod permission;
#[cfg(feature = "gui-shot")]
mod shot;

#[allow(unused_imports)]
pub(super) use people::occupants;

use super::components::{glyph, network_icon, tip};
use super::state::{Shell, Snapshot};
use super::theme;
use crate::client::ask;
use gpui::{
    div, prelude::*, px, Action, AnyElement, App, Context, Div, Entity, FontWeight, Global, KeyBinding,
    MouseButton, Rgba, Stateful, Svg, Task, WeakEntity, Window,
};
use protocol::{
    Call, CallFailure, CallState, CallSupport, Change, ConversationKind, Key, MediaFile, Network,
    Occupants, Request, Response,
};
use std::cell::OnceCell;
use std::mem::{discriminant, Discriminant};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

gpui::actions!(call, [ToggleMute, ToggleDeafen, Leave, Accept, Decline, Jump]);

/// The call shortcuts of 07 §2. None of them is taken by `02-layout-navigation.md`, the text
/// input or the window. They are bound the first time a call view is drawn (`ensure_keys`), so
/// nothing outside this file has to call this.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-m", ToggleMute, None),
        KeyBinding::new("cmd-shift-d", ToggleDeafen, None),
        KeyBinding::new("cmd-shift-h", Leave, None),
        KeyBinding::new("cmd-shift-a", Accept, None),
        KeyBinding::new("cmd-shift-r", Decline, None),
        KeyBinding::new("cmd-shift-j", Jump, None),
    ]);
}

/// Set once the shortcuts are bound and their handlers registered.
struct Bound;

impl Global for Bound {}

fn on<A: Action>(cx: &mut App, shell: &WeakEntity<Shell>, act: fn(&mut Shell, &mut Context<Shell>)) {
    let shell = shell.clone();
    cx.on_action(move |_: &A, cx| {
        let _ = shell.update(cx, act);
    });
}

/// Binds the shortcuts and points them at this shell. Does it once.
fn ensure_keys(cx: &mut Context<Shell>) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    bind_keys(cx);
    let shell = cx.weak_entity();
    on::<ToggleMute>(cx, &shell, Shell::call_mute);
    on::<ToggleDeafen>(cx, &shell, Shell::call_deafen);
    on::<Leave>(cx, &shell, Shell::call_leave);
    on::<Accept>(cx, &shell, Shell::call_accept);
    on::<Decline>(cx, &shell, Shell::call_decline);
    on::<Jump>(cx, &shell, Shell::call_jump);
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The popover open above the dock.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Menu {
    Devices,
    People,
}

/// What a counter in the dock is counting.
#[derive(Clone, PartialEq, Eq)]
enum Counting {
    Ring,
    Joined,
    Reconnect,
}

/// The one number in the dock that moves: how long the call has rung, run or been reconnecting.
/// It is its own entity so a tick is this text's and nobody else's, and it holds the one timer.
/// The timer stops by itself a second after the text was last drawn.
#[derive(Default)]
pub(super) struct Counter {
    what: Option<(Counting, Key)>,
    /// Unix seconds the count starts from.
    base: i64,
    /// Drawn since the last tick.
    seen: bool,
    tick: Option<Task<()>>,
}

impl Counter {
    /// Says what to count. A ring and a reconnect count from when the shell first saw them.
    fn follow(&mut self, what: Option<(Counting, Key)>, joined: i64, cx: &mut Context<Self>) {
        if self.what != what {
            let from_join = matches!(what, Some((Counting::Joined, _))) && joined > 0;
            // The fabricated calls of `--shot` also give a ring a start, to draw `0:12`.
            let from_join = from_join || (cfg!(feature = "gui-shot") && joined > 0);
            self.base = if from_join { joined } else { now() };
            self.what = what;
            cx.notify();
        }
        if self.what.is_none() {
            self.tick = None;
        } else if self.tick.is_none() {
            self.seen = true;
            self.tick = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let shown = this.update(cx, |this, cx| {
                    let shown = std::mem::take(&mut this.seen) && this.what.is_some();
                    if shown {
                        cx.notify();
                    }
                    shown
                });
                if !matches!(shown, Ok(true)) {
                    let _ = this.update(cx, |this, _| drop(this.tick.take()));
                    break;
                }
            }));
        }
    }
}

impl Render for Counter {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.seen = true;
        let seconds = (now() - self.base).max(0);
        let words = match seconds / 3600 {
            0 => format!("{}:{:02}", seconds / 60, seconds % 60),
            hours => format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
        };
        div().flex_none().child(words)
    }
}

/// How long "Call ended" and "Now using ..." stay.
const BRIEFLY: Duration = Duration::from_secs(4);

/// A call that was up, as it was when last heard of: what "Call ended" is written from.
struct Was {
    conversation: Key,
    subject: Subject,
    joined: i64,
    /// The microphone it was using.
    input: String,
    /// Someone else was in it at some point, so being alone now means they left.
    peer: Option<String>,
}

/// The other side ended the call. Shown for a few seconds, or until dismissed.
pub(super) struct Ended {
    pub conversation: Key,
    pub subject: Subject,
    /// How long the call ran, in seconds.
    pub seconds: i64,
}

/// A call to start once the user agrees to leave the one that is up.
#[derive(Clone)]
pub(super) struct Confirm {
    pub conversation: Key,
    pub title: String,
    /// The call to start is a ring to answer, not one to place.
    pub answer: bool,
}

/// What the call views keep between frames, beyond the call itself (`Snapshot::call`).
#[derive(Default)]
pub struct State {
    /// Made the first time the dock is drawn.
    counter: OnceCell<Entity<Counter>>,
    menu: Option<Menu>,
    /// When a click outside closed the menu, so the same click on its button does not reopen it.
    menu_closed: Option<Instant>,
    /// The device picked in this window. The outer `None` is "not picked here".
    input: Option<Option<String>>,
    output: Option<Option<String>>,
    /// Whether I was muted when I deafened, to go back to on undeafen.
    mute_before_deafen: Option<bool>,
    /// What chatd or the adapter said about the last thing asked of the call.
    error: Option<String>,
    /// The call and state `error` is about; it goes when either changes.
    last: Option<(Key, Discriminant<CallState>)>,
    was: Option<Was>,
    /// Leaving was asked for here, so the call going away is not the other side ending it.
    leaving: bool,
    ended: Option<Ended>,
    confirm: Option<Confirm>,
    /// A call that could not be asked for at all: the microphone is not allowed. Drawn as any
    /// failed call is, and gone when dismissed or when the next call starts.
    local: Option<Call>,
    /// "Now using ...", after the system moved the call to another device.
    notice: Option<String>,
    /// Clears `ended` or `notice` when its few seconds are up.
    brief: Option<Task<()>>,
}

/// What a call is of: the conversation's name, kind and picture.
#[derive(Clone)]
pub(super) struct Subject {
    pub title: String,
    pub kind: ConversationKind,
    pub avatar: Option<MediaFile>,
    /// The Space it is in, by name.
    pub space: Option<String>,
}

impl Subject {
    fn of(shell: &Shell, snapshot: &Snapshot, call: &Call) -> Subject {
        #[cfg(feature = "gui-shot")]
        if let Some(subject) = shot::subject(snapshot) {
            return subject;
        }
        let key = &call.conversation;
        let open = snapshot.open.as_ref().filter(|open| &open.key == key).cloned();
        if let Some(row) = open.or_else(|| shell.sidebar_state.find(&shell.trees, key)) {
            // A channel's line names its server; anything else, its network.
            let space = row.space.as_ref().and_then(|id| {
                let found = shell.spaces.iter().find(|space| {
                    &space.key.id == id && space.key.account == key.account && space.key.network == key.network
                });
                found.map(|space| space.name.clone())
            });
            return Subject { title: row.title, kind: row.kind, avatar: row.avatar, space };
        }
        // Not on the first page of the Inbox and not open: say who, from the call itself.
        let title = match (&call.state, call.participants.first()) {
            (CallState::RingingIn { from }, _) => from.clone(),
            (_, Some(first)) => first.name.clone(),
            (_, None) => "Call".to_owned(),
        };
        let kind = match call.participants.len() {
            0 | 1 => ConversationKind::Direct,
            _ => ConversationKind::Group,
        };
        Subject { title, kind, avatar: None, space: None }
    }
}

/// The call the views draw: chatd's, or under `--shot` the one `TINYCLIENT_SHOT_CALL` names.
fn shown(snapshot: &Snapshot) -> Option<Call> {
    #[cfg(feature = "gui-shot")]
    if let Some(call) = shot::call(snapshot) {
        return Some(call);
    }
    snapshot.call.clone()
}

/// The names of who is in a conversation's call, for a tooltip: "Iris, Jun and 1 more".
fn named(occupants: &Occupants) -> String {
    let first = |name: &str| name.split_whitespace().next().unwrap_or(name).to_owned();
    let names: Vec<String> = occupants.people.iter().take(2).map(|who| first(&who.name)).collect();
    let more = (occupants.total as usize).saturating_sub(names.len());
    match (names.as_slice(), more) {
        ([], _) => format!("{} people are in this call", occupants.total),
        ([one], 0) => format!("{one} is in this call"),
        ([one, two], 0) => format!("{one} and {two} are in this call"),
        (names, more) => format!("{} and {more} more are in this call", names.join(", ")),
    }
}

/// Starts the `chatd` beside this binary when the window runs from `tinyclient.app` and no chatd
/// answers. Everything in the bundle is in `Contents/MacOS`, so "beside" is where it is.
///
/// A chatd started by the app is the app's own to macOS: the microphone prompt for a call,
/// which mediad causes two processes down, is then asked in tinyclient's name. Run from a
/// terminal or `target/`, nothing is started: chatd there is whoever started it by hand.
// This is start-up work, done from the first draw of a call view because that is the first of
// this module's code to run. It belongs in the shell's start; move it there when that is cut.
fn ensure_daemon() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(here) = std::env::current_exe() else {
            return;
        };
        let bundled = here.parent().is_some_and(|dir| dir.ends_with("Contents/MacOS"));
        let chatd = here.with_file_name(format!("chatd{}", std::env::consts::EXE_SUFFIX));
        if !bundled || !chatd.exists() {
            return;
        }
        std::thread::spawn(move || {
            let down = matches!(&ask(&Request::Ping), Err(err)
                if matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused));
            if !down {
                return;
            }
            let log = protocol::support_dir().join("chatd.log");
            let _ = std::fs::create_dir_all(protocol::support_dir());
            let out = std::fs::OpenOptions::new().create(true).append(true).open(log);
            let mut command = std::process::Command::new(chatd);
            command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
            if let Ok(out) = out {
                command.stderr(out);
            }
            if let Err(err) = command.spawn() {
                eprintln!("tinyclient: could not start chatd: {err}");
            }
        });
    });
}

/// Joined, or was: the only states drawn as "in a call".
fn in_call(call: &Call) -> bool {
    matches!(call.state, CallState::Joined | CallState::Reconnecting)
}

/// A glyph in a control. GPUI does not hand a parent's text color down to an SVG, so the color
/// is the glyph's own, and `hover` names the control (its group) and the color it takes there.
fn icon(name: &'static str, size: f32, color: Rgba, hover: Option<(&'static str, Rgba)>) -> Svg {
    let icon = glyph(name, size, color);
    match hover {
        Some((group, color)) => icon.group_hover(group, move |style| style.text_color(color)),
        None => icon,
    }
}

fn handoff_label(network: Network) -> String {
    match network {
        Network::Slack => "Open huddle in Slack".to_owned(),
        other => format!("Open in {}", other.label()),
    }
}

/// Hands a link to the system. Only something with a scheme, never an option.
fn open(url: &str) -> Result<(), String> {
    if url.starts_with('-') || !url.contains(':') {
        return Err(format!("Not a link: {url}"));
    }
    match std::process::Command::new("open").arg(url).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(format!("Nothing on this Mac opens {url}")),
        Err(_) => Err("This Mac could not open the link.".to_owned()),
    }
}

/// The dock, in the shape of the sidebar's slot (`layout::Slots::call_dock`).
pub(super) fn dock(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let snapshot = shell.snapshot.as_ref()?;
    shell.call_dock(snapshot, cx).map(IntoElement::into_any_element)
}

impl Shell {
    /// Offered every change chatd reports, after `Shell` has applied its own part.
    pub(super) fn call_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        ensure_keys(cx);
        let Change::Call { call } = change else {
            return;
        };
        let last = call.as_ref().map(|call| (call.conversation.clone(), discriminant(&call.state)));
        if self.call_state.last != last {
            self.call_state.last = last;
            self.call_state.error = None;
        }
        match call {
            None => {
                self.call_menu_set(None, cx);
                let state = &mut self.call_state;
                // A pick in the device menu was for that call; the next starts from Settings.
                state.input = None;
                state.output = None;
                state.mute_before_deafen = None;
                state.confirm = None;
                state.notice = None;
                // A call that was up and went without our asking: the other side ended it.
                let was = state.was.take();
                if let Some(was) = was.filter(|_| !std::mem::take(&mut state.leaving)) {
                    let seconds = (now() - was.joined).max(0);
                    state.ended = Some(Ended { conversation: was.conversation, subject: was.subject, seconds });
                    self.call_briefly(cx);
                }
                self.call_state.leaving = false;
            }
            Some(call) => {
                // A call of chatd's own replaces anything said here about one that never began.
                self.call_state.local = None;
                self.call_state.ended = None;
                let menus = matches!(call.state, CallState::RingingOut | CallState::Connecting | CallState::Joined);
                if !menus {
                    self.call_menu_set(None, cx);
                }
                if !call.deafened {
                    self.call_state.mute_before_deafen = None;
                }
                if matches!(call.state, CallState::RingingIn { .. }) {
                    incoming(call);
                }
                if !in_call(call) {
                    // Ringing, connecting or failed: not a call that can end.
                    self.call_state.was = None;
                    return;
                }
                let subject = self.snapshot.as_ref().map(|snapshot| Subject::of(self, snapshot, call));
                let Some(subject) = subject else {
                    return;
                };
                let before = self.call_state.was.take().filter(|was| was.conversation == call.conversation);
                // The system moved us to another microphone (the headset was unplugged): said
                // for a few seconds, unless it is the device picked here a moment ago.
                let moved = before.as_ref().is_some_and(|was| {
                    !was.input.is_empty() && was.input != call.input && !call.input.is_empty()
                });
                if moved && self.call_state.input.clone().flatten().as_deref() != Some(call.input.as_str()) {
                    self.call_state.notice = Some(format!("Now using {}", call.input));
                    self.call_briefly(cx);
                }
                let peer = call.participants.first().map(|who| who.name.clone());
                self.call_state.was = Some(Was {
                    conversation: call.conversation.clone(),
                    subject,
                    joined: call.joined,
                    input: call.input.clone(),
                    peer: peer.or(before.and_then(|was| was.peer)),
                });
            }
        }
    }

    /// Lets "Call ended" or "Now using ..." stand for a few seconds, then takes it away.
    fn call_briefly(&mut self, cx: &mut Context<Self>) {
        self.call_state.brief = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BRIEFLY).await;
            let _ = this.update(cx, |this, cx| {
                this.call_state.ended = None;
                this.call_state.notice = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The name of whoever was in this call with me and is not any more.
    pub(super) fn call_left_alone(&self, call: &Call) -> Option<String> {
        if call.state != CallState::Joined || !call.participants.is_empty() {
            return None;
        }
        #[cfg(feature = "gui-shot")]
        if let Some(left) = shot::left() {
            return Some(left);
        }
        let was = self.call_state.was.as_ref().filter(|was| was.conversation == call.conversation)?;
        was.peer.clone()
    }

    /// One request about the call, asked off the main thread. chatd answers the outcome as a
    /// `Change::Call`; only a refusal is kept, to be shown in the dock and by the composer.
    fn call_ask(&mut self, request: Request, cx: &mut Context<Self>) {
        self.call_state.error = None;
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words(&err)),
            };
            let _ = this.update(cx, |this, cx| {
                if refused.is_some() {
                    this.call_state.error = refused.clone();
                    this.refused = refused;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn call_now(&self) -> Option<&Call> {
        self.snapshot.as_ref()?.call.as_ref()
    }

    /// The call the dock draws: chatd's, else the one that could not even be asked for.
    pub(super) fn call_shown(&self, snapshot: &Snapshot) -> Option<Call> {
        shown(snapshot).or_else(|| self.call_state.local.clone())
    }

    fn call_set(&mut self, muted: bool, deafened: bool, cx: &mut Context<Self>) {
        self.call_ask(Request::CallSet { muted, deafened }, cx);
    }

    /// ⌘⇧M. Unmuting while deafened also undeafens: I cannot talk to people I cannot hear.
    pub(super) fn call_mute(&mut self, cx: &mut Context<Self>) {
        let Some(call) = self.call_now() else {
            return;
        };
        if !matches!(call.state, CallState::RingingOut | CallState::Connecting | CallState::Joined) {
            return;
        }
        let (muted, deafened) = (call.muted, call.deafened);
        if deafened {
            self.call_state.mute_before_deafen = None;
            self.call_set(false, false, cx);
        } else {
            self.call_set(!muted, false, cx);
        }
    }

    /// ⌘⇧D. Deafening mutes too; undeafening puts the mute back as it was.
    pub(super) fn call_deafen(&mut self, cx: &mut Context<Self>) {
        let Some(call) = self.call_now() else {
            return;
        };
        if call.state != CallState::Joined {
            return;
        }
        let (muted, deafened) = (call.muted, call.deafened);
        if deafened {
            let before = self.call_state.mute_before_deafen.take().unwrap_or(muted);
            self.call_set(before, false, cx);
        } else {
            self.call_state.mute_before_deafen = Some(muted);
            self.call_set(true, true, cx);
        }
    }

    /// ⌘⇧H: leave, cancel, or dismiss a failure. A ring coming in is declined, not left.
    pub(super) fn call_leave(&mut self, cx: &mut Context<Self>) {
        // What only this window shows goes without asking anyone.
        let state = &mut self.call_state;
        if state.local.take().is_some() | state.ended.take().is_some() | state.confirm.take().is_some() {
            cx.notify();
        }
        match self.call_now().map(|call| &call.state) {
            None | Some(CallState::RingingIn { .. }) => {}
            Some(_) => {
                self.call_state.leaving = true;
                self.call_ask(Request::CallEnd, cx);
            }
        }
    }

    /// ⌘⇧A.
    pub(super) fn call_accept(&mut self, cx: &mut Context<Self>) {
        let ringing = self.call_now().filter(|call| matches!(call.state, CallState::RingingIn { .. }));
        if let Some(conversation) = ringing.map(|call| call.conversation.clone()) {
            self.call_begin(vec![Request::CallAccept], conversation, cx);
        }
    }

    /// ⌘⇧R.
    pub(super) fn call_decline(&mut self, cx: &mut Context<Self>) {
        if matches!(self.call_now().map(|call| &call.state), Some(CallState::RingingIn { .. })) {
            self.call_ask(Request::CallDecline, cx);
        }
    }

    /// ⌘⇧J: opens the conversation the call is in.
    pub(super) fn call_jump(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(call) = self.call_shown(snapshot) else {
            return;
        };
        let title = Subject::of(self, snapshot, &call).title;
        self.select(call.conversation, &title, cx);
    }

    /// Rings or joins. A failed call in the dock is replaced by the new one. A call that is up
    /// somewhere else is not left without a word: the dock asks first.
    pub(super) fn call_start(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let busy = self.call_now().filter(|call| {
            call.conversation != conversation && !matches!(call.state, CallState::Failed { .. } | CallState::RingingIn { .. })
        });
        if busy.is_some() {
            let known = self.snapshot.as_ref().and_then(|snapshot| snapshot.open.as_ref()).filter(|open| open.key == conversation);
            let row = known.cloned().or_else(|| self.sidebar_state.find(&self.trees, &conversation));
            let title = row.map_or_else(|| "this call".to_owned(), |row| row.title);
            self.call_state.confirm = Some(Confirm { conversation, title, answer: false });
            self.call_menu_set(None, cx);
            cx.notify();
            return;
        }
        self.call_begin(vec![Request::CallStart { conversation: conversation.clone() }], conversation, cx);
    }

    /// "Leave & call": leaves the call that is up, then starts the one that was asked for.
    pub(super) fn call_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.call_state.confirm.take() else {
            return;
        };
        self.call_state.leaving = true;
        let next = if confirm.answer {
            Request::CallAccept
        } else {
            Request::CallStart { conversation: confirm.conversation.clone() }
        };
        self.call_begin(vec![Request::CallEnd, next], confirm.conversation, cx);
    }

    pub(super) fn call_stay(&mut self, cx: &mut Context<Self>) {
        self.call_state.confirm = None;
        cx.notify();
    }

    /// Makes sure the microphone may be used, then asks chatd for `requests` in order. All of
    /// it off the main thread: the system's prompt can stay up as long as the user likes.
    fn call_begin(&mut self, requests: Vec<Request>, conversation: Key, cx: &mut Context<Self>) {
        let state = &mut self.call_state;
        (state.error, state.local, state.ended) = (None, None, None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let work = async move {
                // A picture of the dock never asks anyone for the microphone.
                let allowed = cfg!(feature = "gui-shot") || permission::ensure() != permission::Microphone::Denied;
                if !allowed {
                    return Err(None);
                }
                for request in &requests {
                    match ask(request) {
                        Ok(Response::Error { detail, .. }) => return Err(Some(detail)),
                        Ok(_) => {}
                        Err(err) => return Err(Some(crate::client::trouble_words(&err))),
                    }
                }
                Ok(())
            };
            let outcome = cx.background_executor().spawn(work).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {}
                    Err(Some(refused)) => {
                        this.call_state.error = Some(refused.clone());
                        this.refused = Some(refused);
                    }
                    // Denied: no call was asked for. The dock says so, as it says any failure.
                    Err(None) => {
                        let reason = CallFailure::Audio;
                        let state = CallState::Failed { reason, detail: permission::DENIED.to_owned() };
                        this.call_state.local = Some(Call {
                            conversation,
                            state,
                            participants: Vec::new(),
                            muted: false,
                            deafened: false,
                            joined: 0,
                            input: String::new(),
                            output: String::new(),
                            engine_linked: true,
                            video: Vec::new(),
                            speaking: false,
                            level: None,
                            inputs: Vec::new(),
                            outputs: Vec::new(),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks where the official app takes this conversation's call and opens it there. mediad is
    /// never started for this. What the adapter refuses is shown, not swallowed.
    pub(super) fn call_handoff(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.call_state.error = None;
        cx.spawn(async move |this, cx| {
            let work = async move {
                match ask(&Request::Handoff { conversation }) {
                    Ok(Response::Link { url }) => open(&url),
                    Ok(Response::Error { detail, .. }) => Err(detail),
                    Ok(_) => Err(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => Err(crate::client::trouble_words(&err)),
                }
            };
            let refused = cx.background_executor().spawn(work).await.err();
            let _ = this.update(cx, |this, cx| {
                if refused.is_some() {
                    this.call_state.error = refused.clone();
                    this.refused = refused;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The header pill, by what the open conversation can do (07 §1). Its look is the capability:
    /// a filled pill calls from here, an outlined one with the network's icon opens the official
    /// app, and a conversation that can do neither has none.
    pub(super) fn call_pill(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        ensure_keys(cx);
        ensure_daemon();
        let open = snapshot.open.as_ref()?;
        let key = open.key.clone();
        let mine = shown(snapshot).filter(|call| {
            call.conversation == key && !matches!(call.state, CallState::Failed { .. })
        });
        let pill = div()
            .id("call")
            .group("call-pill")
            .flex_none()
            .h(px(30.))
            .px(px(12.))
            .rounded(px(15.))
            .border_1()
            .flex()
            .items_center()
            .gap(px(7.))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            // Keep the press from also dragging the window.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        if let Some(call) = mine {
            let incoming = matches!(call.state, CallState::RingingIn { .. });
            let word = match call.state {
                CallState::RingingIn { .. } => "Decline",
                CallState::RingingOut | CallState::Connecting => "Cancel",
                _ => "Leave",
            };
            let hint = if incoming { ("Decline the call", "⌘⇧R") } else { ("Leave the call", "⌘⇧H") };
            return Some(
                pill.bg(theme::danger_soft())
                    .border_color(gpui::transparent_black())
                    .text_color(theme::danger())
                    .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
                    .tooltip(tip(hint.0, Some(hint.1)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if incoming {
                            this.call_decline(cx);
                        } else {
                            this.call_leave(cx);
                        }
                    }))
                    .child(icon("icons/phone-off.svg", 14., theme::danger(), Some(("call-pill", theme::on_accent()))))
                    .child(word),
            );
        }
        let network = key.network;
        match open.call {
            CallSupport::None => None,
            CallSupport::Native => {
                let voice = open.kind == ConversationKind::Voice;
                // A direct or group conversation whose call is already running: joining it
                // rings nobody. The adapter lists who is in it as the row's occupants.
                let running = open.occupants.as_ref().filter(|there| !voice && there.total > 0);
                #[cfg(feature = "gui-shot")]
                let shot = shot::running();
                #[cfg(feature = "gui-shot")]
                let running = shot.as_ref().or(running);
                if let Some(there) = running {
                    return Some(
                        pill.bg(theme::accent_soft())
                            .border_color(gpui::transparent_black())
                            .text_color(theme::accent())
                            .hover(|style| style.bg(theme::accent_wash()))
                            .tooltip(tip(named(there), None))
                            .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                            .child(icon("icons/phone.svg", 14., theme::accent(), None))
                            .child(format!("Join call · {}", there.total)),
                    );
                }
                let (glyph, word, hint) = if voice {
                    (
                        "icons/volume-2.svg",
                        "Join voice",
                        format!("Join {}. People there will see you arrive.", open.title),
                    )
                } else {
                    ("icons/phone.svg", "Call", format!("Call {} on {}", open.title, network.label()))
                };
                Some(
                    pill.bg(theme::raised())
                        .border_color(theme::hairline())
                        .text_color(theme::text())
                        .hover(|style| style.bg(theme::selected()))
                        .tooltip(tip(hint, None))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                        .child(icon(glyph, 14., theme::text(), None))
                        .child(word),
                )
            }
            // Never says "Call": it is a different object, and it starts nothing here.
            CallSupport::Handoff => Some(
                pill.border_color(theme::border())
                    .text_color(theme::text_soft())
                    .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                    .tooltip(tip(
                        format!(
                            "{0} calls open in the {0} app. tinyclient doesn't carry them.",
                            network.label()
                        ),
                        None,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_handoff(key.clone(), cx)))
                    .child(network_icon(network, 14.))
                    .child(handoff_label(network))
                    .child(icon("icons/arrow-up-right.svg", 12., theme::text_faint(), None)),
            ),
        }
    }
}

/// A ring just arrived. Where the system notification of 07 §4 will be posted from.
///
/// Not built this round. What it needs: the process that posts must run inside the signed
/// bundle (`packaging/bundle.sh` makes it; `UNUserNotificationCenter` refuses a bare binary),
/// and by the design it is chatd that posts, so the ring arrives with the window closed. chatd
/// is in the bundle now, started by the window (`ensure_daemon`); the matching place there is
/// `Calls::ring` and `Calls::ring_stopped` in `chatd/src/media.rs`. Title: the caller, or the
/// group. Subtitle: "Discord · Incoming call". Actions Accept and Decline map to `CallAccept`
/// and `CallDecline`. The notification is withdrawn on `ring_stopped`.
fn incoming(_call: &Call) {}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Occupant;

    fn there(names: &[&str], total: u32) -> Occupants {
        let person = |name: &&str| Occupant {
            person: Key { network: Network::Discord, account: "1".into(), id: name.to_lowercase() },
            name: (*name).to_owned(),
            muted: false,
            deafened: false,
            streaming: false,
        };
        Occupants { people: names.iter().map(person).collect(), total }
    }

    #[test]
    fn the_join_pill_says_who_is_in_the_call() {
        assert_eq!(named(&there(&["Iris Navarro"], 1)), "Iris is in this call");
        assert_eq!(named(&there(&["Iris Navarro", "Jun Aoki"], 2)), "Iris and Jun are in this call");
        assert_eq!(named(&there(&["Iris Navarro", "Jun Aoki", "Dev Patel"], 3)), "Iris, Jun and 1 more are in this call");
        assert_eq!(named(&there(&[], 4)), "4 people are in this call");
    }
}
