//! Calls in the shell (`docs/design/07-calls.md`): the header pill that starts, leaves or hands
//! off a call, and the dock at the bottom of the sidebar that is every state of one. The dock is
//! in `dock`, its device menu in `devices`, who is in the call in `people`.
//!
//! Nothing here talks to chatd on the main thread, and nothing is drawn as "in a call" before
//! chatd says `Joined`.

mod devices;
mod dock;
pub(in crate::gui) mod health;
mod people;
mod permission;
pub(crate) mod phone;
pub(in crate::gui) mod popout;
pub(in crate::gui) mod security;
#[cfg(feature = "gui-shot")]
mod shot;
pub(in crate::gui) mod stage;
pub(in crate::gui) mod view;

// The platform's side of a phone call: the mobile layer routes these (`phone::hooks`).
#[allow(unused_imports)]
pub(crate) use phone::hooks::{
    note_permission, open_system_settings, route_audio, route_flip, route_system_settings, Permission, Route,
};

#[allow(unused_imports)]
pub(super) use people::occupants;
pub(super) use people::ringed;

use super::components::{glyph, menu_item, menu_label, menu_separator, network_icon, slider, tip};
use super::state::{Shell, Snapshot};
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{
    canvas, div, prelude::*, px, Action, AnyElement, App, Bounds, ClickEvent, Context, Div, Entity,
    FontWeight, Global, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Rgba, SharedString, Stateful, Svg, Task, WeakEntity, Window,
};
use protocol::settings::MUTED_TALK;
use protocol::{
    AccountKey, Call, CallFailure, CallState, CallSupport, Change, ConversationKind, Key,
    MediaFile, Network, Occupants, Request, Response,
};
use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::mem::{discriminant, Discriminant};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

gpui::actions!(call, [ToggleMute, ToggleDeafen, Leave, Accept, Decline, Jump, TogglePopout]);

/// The call shortcuts of 07 §2. None of them is taken by `02-layout-navigation.md`, the text
/// input or the window. They are bound the first time a call view is drawn (`ensure_keys`), so
/// nothing outside this file has to call this.
pub(super) fn bind_keys(cx: &mut App) {
    macro_rules! bind {
        ($id:literal, $action:expr) => {
            for chord in super::keymap::chords($id) {
                cx.bind_keys([KeyBinding::new(&chord, $action, None)]);
            }
        };
    }
    bind!("mute", ToggleMute);
    bind!("deafen", ToggleDeafen);
    bind!("leave-call", Leave);
    bind!("accept", Accept);
    bind!("decline", Decline);
    bind!("go-to-call", Jump);
    bind!("popout-call", TogglePopout);
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
    on::<TogglePopout>(cx, &shell, Shell::call_toggle_popout);
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The popover open above the dock.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Menu {
    Devices,
    /// Camera picker and live preview before turning the camera on.
    CameraPreview,
    People,
    /// Add friends to a Discord 1:1 or group DM call.
    AddPeople,
    /// How the call is protected (`security`).
    Security,
    /// Per-person volume and local mute for someone in any call.
    Person { account: AccountKey, user_id: String, name: String },
}

/// What a counter in the dock is counting.
#[derive(Clone, PartialEq, Eq)]
enum Counting {
    Ring,
    Connect,
    Joined,
    /// Counts up: how long the call has been reconnecting.
    Recover,
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
    /// Says what to count. A ring, a join and a reconnect count from `since`, when the shell
    /// first saw that state, so every counter of one call says the same.
    fn follow(&mut self, what: Option<(Counting, Key)>, joined: i64, since: i64, cx: &mut Context<Self>) {
        if self.what != what {
            let from_join = matches!(what, Some((Counting::Joined, _))) && joined > 0;
            // The fabricated calls of `--shot` also give a ring a start, to draw `0:12`.
            let from_join = from_join || (cfg!(feature = "gui-shot") && joined > 0);
            self.base = if from_join { joined } else { since };
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
        let clock = |seconds: i64| match seconds / 3600 {
            0 => format!("{}:{:02}", seconds / 60, seconds % 60),
            hours => format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
        };
        div().flex_none().child(clock(seconds))
    }
}

/// How long "Call ended" and "Now using ..." stay.
const BRIEFLY: Duration = Duration::from_secs(4);
/// A room change this soon after a join was asked for here is that join, not a move.
const MOVE_ASKED: Duration = Duration::from_secs(10);
/// How often the meter is asked for again while muted, should something have turned it off.
const METER_AGAIN: Duration = Duration::from_secs(2);
/// The call went from one room of a Tiny server, or one Discord voice channel, to another of the same account.
fn moved(from: &Key, to: &Key) -> bool {
    from != to && from.network == to.network && from.account == to.account && matches!(from.network, Network::Tiny | Network::Discord)
}

/// Turns the call's microphone meter on or off. Nothing is shown if it is refused.
fn meter(on: bool, cx: &mut Context<Shell>) {
    cx.background_executor()
        .spawn(async move {
            let _ = ask(&Request::Media(protocol::MediaRequest::Meter { on }));
        })
        .detach();
}

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

/// Issue chips offered for ratings below five stars (Telegram call rating, #447).
pub(super) const CALL_RATING_REASONS: &[&str] = &[
    "Echo",
    "Noise",
    "Interruptions",
    "Distorted speech",
    "Silence",
    "Dropped",
];

/// How long an untouched post-call rating card stays in the dock before going away on its own.
const RATING_IDLE: Duration = Duration::from_secs(45);

/// Post-call rating card shown in the dock when Telegram asks (`need_rating`) or when the user
/// clicks Rate on a finished call's row. Never blocks starting the next call.
pub(super) struct CallRatingCard {
    pub prompt: protocol::CallRatingPrompt,
    pub title: String,
    pub user_initiative: bool,
    pub stars: u8,
    pub reasons: Vec<&'static str>,
    pub comment_input: Entity<super::input::TextInput>,
    pub sending: bool,
    pub error: Option<String>,
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
    /// The frames of the picture being watched.
    pub(in crate::gui) feed: super::tiny::feed::Feed<std::sync::Arc<gpui::RenderImage>>,
    /// Per-source frames for multi-stream video receive, keyed by `(user_id, kind)`.
    pub(in crate::gui) feeds: HashMap<(String, protocol::PictureKind), super::tiny::feed::Feed<std::sync::Arc<gpui::RenderImage>>>,
    /// The frames of my own camera, for my tile.
    pub(in crate::gui) self_feed: super::tiny::feed::Feed<std::sync::Arc<gpui::RenderImage>>,
    /// The frames of my own share, for my stream's tile and the stage.
    pub(in crate::gui) screen_feed: super::tiny::feed::Feed<std::sync::Arc<gpui::RenderImage>>,
    /// Made the first time the dock is drawn.
    counter: OnceCell<Entity<Counter>>,
    /// The same count, for a Tiny channel's stage: a second text, ticking on its own.
    stage_counter: OnceCell<Entity<Counter>>,
    /// The same count again, for the phone's call screen and its call bar (one shows at a time).
    phone_counter: OnceCell<Entity<Counter>>,
    /// What the phone's call screen keeps: minimised or not, its page and sheets, the camera tile.
    pub(in crate::gui) phone: phone::State,
    /// Talking while muted: what the meter heard, and the redraw that takes the pill away.
    muted_talk: health::MutedTalk,
    muted_talk_task: Option<Task<()>>,
    /// The meter was turned on here to hear talking while muted (not by the device menu), and
    /// when it was last asked for.
    metering: Option<Instant>,
    /// When a call or join was last asked for here.
    asked: Option<Instant>,
    /// The call whose joining was chimed: a move or a reconnect does not chime it again.
    cued: Option<Key>,
    menu: Option<Menu>,
    /// Which security indicator opened `Menu::Security`: the panel opens beside it.
    security_at: security::Place,
    /// Whether the security panel's "More about this call" disclosure is open.
    security_more: bool,
    /// When a click outside closed the menu, so the same click on its button does not reopen it.
    menu_closed: Option<Instant>,
    /// What mediad last said is not as chosen (`Call::device_notice`), so a new one is shown.
    device_notice: Option<String>,
    /// Whether I was muted when I deafened, to go back to on undeafen.
    mute_before_deafen: Option<bool>,
    /// What chatd or the adapter said about the last thing asked of the call.
    error: Option<String>,
    /// The call and state `error` is about; it goes when either changes.
    last: Option<(Key, Discriminant<CallState>)>,
    /// Unix seconds `last` began: where a ring, a connect or a reconnect counts from.
    since: i64,
    was: Option<Was>,
    /// Leaving was asked for here, so the call going away is not the other side ending it.
    leaving: bool,
    ended: Option<Ended>,
    /// Post-call rating prompt (`Change::CallRating`) or user-initiated row rating.
    pub(super) rating: Option<CallRatingCard>,
    /// Auto-dismisses an untouched `rating` card after [`RATING_IDLE`].
    rating_timer: Option<Task<()>>,
    confirm: Option<Confirm>,
    /// A call that could not be asked for at all: the microphone is not allowed. Drawn as any
    /// failed call is, and gone when dismissed or when the next call starts.
    local: Option<Call>,
    /// The call just asked for (a join, an answer) while chatd has not yet said anything about
    /// it: drawn as Connecting at once, and gone when chatd's own word arrives or the ask is
    /// answered. Never acted on: mute, stats and the like speak only to chatd's call.
    pub(in crate::gui) connecting: Option<Call>,
    /// "Now using ...", after the system moved the call to another device; or why a device in
    /// use is not the one chosen, or is not working (`Call::device_notice`).
    notice: Option<String>,
    /// Clears `ended` or `notice` when its few seconds are up.
    brief: Option<Task<()>>,
    /// Per-person volume and local mute, keyed by `(AccountKey, user_id)` so it works in any call
    /// regardless of which server page is open.
    pub(in crate::gui) gains: HashMap<(AccountKey, String), (u16, bool)>,
    /// The volume slider's track bounds, as last drawn, and whose volume is being dragged.
    pub(in crate::gui) slider: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(in crate::gui) sliding: Option<(AccountKey, String)>,
    /// `(conversation, peer_id)` that `call_auto_watch` already asked to watch, so we ask once
    /// per start and respect a manual "Stop watching" until the peer stops and starts again.
    pub(in crate::gui) auto_watch: Option<(Key, String)>,
    /// The Discord participant whose camera we last asked mediad to paint (`MediaRequest::Speaker`),
    /// and when we switched to them (for 800 ms speaker debounce).
    pub(in crate::gui) discord_speaker: Option<(String, Instant)>,
    /// When the user asked to watch a specific stream, so the tile can escalate past "Connecting…"
    /// at 3 s and 10 s without waiting for a frame to arrive.
    pub(in crate::gui) watch_asked: Option<(String, Instant)>,
    watch_timer: Option<Task<()>>,
    /// The call whose full call view has the main pane (`gui/call/view.rs`).
    pub(in crate::gui) view: Option<Key>,
    /// The person drawn large, or the stream filling the stage, pinned by a click on its tile.
    pub(in crate::gui) focus: Option<stage::Focus>,
    /// Where the people go while a stream fills the stage.
    pub(in crate::gui) strip: stage::Strip,
    /// The focused stream has the whole window. True when the window was made full screen for
    /// it, and so goes back when it ends.
    pub(in crate::gui) full: Option<bool>,
    /// True once the user has turned their camera on at least once in this session.
    pub(in crate::gui) camera_used: bool,
    /// The active remote sharer we last saw in this call, so dismissing the notice strip holds
    /// until they stop sharing and someone starts again.
    pub(super) sharer: Option<String>,
    /// The sharer currently offered on the dock's notice strip (cleared on Watch, dismiss, stop,
    /// or call end).
    pub(super) share_prompt: Option<String>,
    /// Last multi-stream watch list the server accepted for this call.
    pub(in crate::gui) watched: Vec<protocol::WatchSource>,
    /// The list in flight. It is not applied until chatd says ok.
    watch_pending: Option<Vec<protocol::WatchSource>>,
    /// Bumped when the call changes, so a late answer cannot apply.
    watch_epoch: u64,
    /// True once `MediaRequest::Watch` has been sent on this call. ShareWatch is then not the
    /// Tiny subscription.
    watch_sent: bool,
    /// A refused list, and when the next picture frame may ask for it again. A click clears it.
    watch_again: Option<(Vec<protocol::WatchSource>, Instant)>,
    /// Always-on-top pop-out window showing the call's video (`#267`). While `Some`, the main
    /// stage renders a placeholder so the decoded `RenderImage` is never drawn twice.
    pub(in crate::gui) popout: Option<gpui::WindowHandle<popout::CallPopout>>,
    /// Remembered `(x, y, width, height)` for the call pop-out window, persisted in `shell.prefs`.
    pub(in crate::gui) popout_frame: Option<(f32, f32, f32, f32)>,
}

/// Snapshot of the active call and its already-decoded frames for [`popout::CallPopout`].
pub(super) struct PopoutSnapshot {
    pub title: String,
    pub muted: bool,
    pub camera_on: bool,
    pub share_on: bool,
    pub can_pictures: bool,
    pub main_frame: Option<std::sync::Arc<gpui::RenderImage>>,
    pub pip_frame: Option<std::sync::Arc<gpui::RenderImage>>,
    pub speaker_name: String,
    pub speaker_speaking: bool,
    pub live_badge: bool,
}

const DISCORD_SPEAKER_DEBOUNCE: Duration = Duration::from_millis(800);
/// How long a refused watch list waits before the next picture frame may ask again.
const WATCH_AGAIN: Duration = Duration::from_secs(2);

/// ShareWatch replaces mediad's source list with one share. On Tiny, once `call.watch` has been
/// sent, that list is the subscription.
fn share_watch_superseded(network: Network, watch_sent: bool) -> bool {
    network == Network::Tiny && watch_sent
}

/// Pure camera-speaker selection for Discord calls (where the shell names whose camera to paint).
/// Returns `Some(Some(id))` to switch camera speaker, `Some(None)` to clear, or `None` for no change.
fn discord_speaker_step(
    call: &Call,
    focused: Option<&str>,
    current: Option<(&str, Instant)>,
    now: Instant,
) -> Option<Option<String>> {
    if call.state != CallState::Joined || call.conversation.network != Network::Discord {
        return None;
    }
    let cur_id = current.map(|(id, _)| id);
    let cur_still_camera = cur_id.is_some_and(|id| call.participants.iter().any(|p| p.id == id && p.camera));
    let target = if let Some(foc) = focused.filter(|id| call.participants.iter().any(|p| p.id == *id && p.camera)) {
        Some(foc)
    } else if let Some(speaker) = call.participants.iter().find(|p| p.camera && p.speaking) {
        if cur_id == Some(speaker.id.as_str()) {
            return None;
        }
        if cur_still_camera && current.is_some_and(|(_, at)| now.saturating_duration_since(at) < DISCORD_SPEAKER_DEBOUNCE) {
            return None;
        }
        Some(speaker.id.as_str())
    } else if cur_still_camera {
        return None;
    } else {
        call.participants.iter().find(|p| p.camera).map(|p| p.id.as_str())
    };
    match target {
        Some(id) if cur_id == Some(id) => None,
        Some(id) => Some(Some(id.to_owned())),
        None if cur_id.is_some() => Some(None),
        None => None,
    }
}

/// A decode of this person was asked for, or their picture is on screen.
pub(in crate::gui) fn watching(call: &Call, user_id: &str) -> bool {
    call.video.iter().any(|stream| {
        stream.direction == protocol::Direction::Decode
            && stream.content == protocol::Content::Share
            && stream.source == user_id
    }) || call
        .pictures
        .iter()
        .any(|picture| !picture.path.is_empty() && picture.user_id == user_id)
        || call
            .picture
            .as_ref()
            .is_some_and(|picture| !picture.path.is_empty() && picture.user_id == user_id)
}

/// Derives the desired multi-stream watch list from the joined Tiny call and current stage focus:
/// focused stream or person is `Large`, and visible remote cameras up to `budget_small` are `Small`.
fn desired_watch_sources(
    call: &Call,
    focus: Option<&stage::Focus>,
    budget_small: usize,
) -> Vec<protocol::WatchSource> {
    use protocol::{PictureKind, WatchSize, WatchSource};
    if call.state != CallState::Joined || call.deafened || call.conversation.network != Network::Tiny {
        return Vec::new();
    }
    let mut out: Vec<WatchSource> = Vec::new();
    fn push(out: &mut Vec<WatchSource>, user_id: &str, kind: PictureKind, size: WatchSize) {
        if !out.iter().any(|s| s.user_id == user_id && s.kind == kind) {
            out.push(WatchSource { user_id: user_id.to_owned(), kind, size });
        }
    }
    match focus {
        Some(stage::Focus::Stream(uid)) => {
            if let Some(p) = call.participants.iter().find(|p| !p.ringing && p.id == *uid) {
                if p.sharing {
                    push(&mut out, &p.id, PictureKind::Share, WatchSize::Large);
                } else if p.camera {
                    push(&mut out, &p.id, PictureKind::Camera, WatchSize::Large);
                }
            }
        }
        Some(stage::Focus::Person(uid)) => {
            if let Some(p) = call.participants.iter().find(|p| !p.ringing && p.id == *uid && p.camera) {
                push(&mut out, &p.id, PictureKind::Camera, WatchSize::Large);
            }
        }
        None => {
            // In a 1:1 call, give the peer's screen or camera the primary Large slot.
            if call.participants.len() == 1 {
                let p = &call.participants[0];
                if !p.ringing {
                    if p.sharing {
                        push(&mut out, &p.id, PictureKind::Share, WatchSize::Large);
                    } else if p.camera {
                        push(&mut out, &p.id, PictureKind::Camera, WatchSize::Large);
                    }
                }
            }
        }
    }
    let mut small = 0usize;
    for p in call.participants.iter().filter(|p| !p.ringing && p.camera) {
        if small >= budget_small {
            break;
        }
        if out.iter().any(|s| s.user_id == p.id && s.kind == PictureKind::Camera) {
            continue;
        }
        push(&mut out, &p.id, PictureKind::Camera, WatchSize::Small);
        small += 1;
    }
    out
}

/// Pure auto-watch decision for a 1:1 Tiny direct call.
/// Returns `Some(Some(peer_id))` to start watching, `Some(None)` to stop watching, or `None` for no-op.
fn auto_watch_step(
    call: &Call,
    is_room: bool,
    prev: &mut Option<(Key, String)>,
) -> Option<Option<String>> {
    if call.state != CallState::Joined
        || call.conversation.network != Network::Tiny
        || is_room
        || call.participants.len() != 1
    {
        if prev.as_ref().is_some_and(|(k, _)| k == &call.conversation) {
            *prev = None;
            return Some(None);
        }
        return None;
    }
    let peer = &call.participants[0];
    if peer.sharing || peer.camera {
        let target = (call.conversation.clone(), peer.id.clone());
        if prev.as_ref() == Some(&target) {
            return None;
        }
        *prev = Some(target);
        if !watching(call, &peer.id) {
            return Some(Some(peer.id.clone()));
        }
        return None;
    }
    if prev.take().is_some() {
        return Some(None);
    }
    None
}

/// Diffs remote screen sharing in a joined call.
/// Returns `Some(user_id)` once when a remote participant starts sharing (or another sharer
/// replaces the previous one). Dismissing the notice clears `prompt` while `prev_sharer` holds
/// until nobody is sharing.
fn share_notice_step(
    call: &Call,
    prev_sharer: &mut Option<String>,
    prompt: &mut Option<String>,
) -> Option<String> {
    if call.state != CallState::Joined {
        *prev_sharer = None;
        *prompt = None;
        return None;
    }
    let current = call
        .participants
        .iter()
        .find(|p| !p.ringing && p.sharing)
        .map(|p| p.id.clone());
    match current {
        None => {
            *prev_sharer = None;
            *prompt = None;
            None
        }
        Some(id) => {
            if prev_sharer.as_deref() == Some(id.as_str()) {
                None
            } else {
                *prev_sharer = Some(id.clone());
                *prompt = Some(id.clone());
                Some(id)
            }
        }
    }
}

/// Whether a newly started screen share should open automatically on the stage:
/// the setting is on, the user is already looking at the call, and nothing is focused yet.
fn should_auto_watch_share(auto_watch_pref: bool, looking_at_call: bool, already_focused: bool) -> bool {
    auto_watch_pref && looking_at_call && !already_focused
}

/// Someone's volume and mute among what chatd keeps, else 100 % and heard.
pub(in crate::gui) fn kept_gain(kept: &[(String, u16, bool)], user_id: &str) -> (u16, bool) {
    kept.iter().find(|(id, _, _)| id == user_id).map_or((100, false), |(_, percent, muted)| (*percent, *muted))
}

impl State {
    pub(in crate::gui) fn gain(&self, account: &AccountKey, user_id: &str) -> (u16, bool) {
        if let Some(set) = self.gains.get(&(account.clone(), user_id.to_owned())) {
            return *set;
        }
        kept_gain(&crate::gui::app::prefs::values().gains(account), user_id)
    }
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
        // A Tiny room is no conversation: its name is on the server's page.
        if key.network == Network::Tiny {
            if let Some((room, server)) = shell.tiny_room(key) {
                return Subject { title: room, kind: ConversationKind::Voice, avatar: None, space: Some(server) };
            }
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
/// A call as this window makes it up before chatd has one: nobody else in it, nothing open.
fn bare_call(conversation: Key, state: CallState, muted: bool, deafened: bool) -> Call {
    Call {
        conversation,
        state,
        participants: Vec::new(),
        muted,
        deafened,
        server_muted: false,
        server_deafened: false,
        suppressed: false,
        joined: 0,
        input: String::new(),
        output: String::new(),
        engine_linked: true,
        video: Vec::new(),
        speaking: false,
        level: None,
        output_level: None,
        input_id: String::new(),
        output_id: String::new(),
        device_notice: None,
        device_stuck: false,
        inputs: Vec::new(),
        outputs: Vec::new(),
        cameras: Vec::new(),
        camera_id: String::new(),
        security: None,
        picture: None,
        pictures: Vec::new(),
        watch_refused: Vec::new(),
        self_view: None,
        share_view: None,
        picture_source: None,
        video_allowed: true,
    }
}

/// Whether the join drawn as Connecting can be dropped now that its ask was answered: the ask
/// failed, or chatd already reports a live call. Otherwise chatd's word has not reached this
/// window yet (or is still a failure left from the call before), and the drawing stays a
/// moment so the dock does not blink out.
fn connecting_settled(worked: bool, chatd: Option<&Call>) -> bool {
    !worked || chatd.is_some_and(|call| !matches!(call.state, CallState::Failed { .. }))
}

/// The grace for the join in `kept` is up: drops the Connecting drawing if it is still that
/// join's, and leaves a later join's alone. True when something was dropped.
fn expire_connecting(connecting: &mut Option<Call>, kept: Option<&Key>) -> bool {
    match (connecting.as_ref(), kept) {
        (Some(call), Some(kept)) if call.conversation == *kept => connecting.take().is_some(),
        _ => false,
    }
}

/// What the dock draws: chatd's call, else what only this window knows. A join asked for just
/// now is drawn over a failure still on show from the call before it, but never over a call
/// that is live.
fn drawn(chatd: Option<Call>, local: Option<&Call>, connecting: Option<&Call>) -> Option<Call> {
    let failed = |call: &Call| matches!(call.state, CallState::Failed { .. });
    match (chatd, connecting) {
        (Some(call), Some(asked)) if failed(&call) => Some(asked.clone()),
        (Some(call), _) => Some(call),
        (None, _) => local.cloned().or_else(|| connecting.cloned()),
    }
}

fn shown(snapshot: &Snapshot) -> Option<Call> {
    #[cfg(feature = "gui-shot")]
    if let Some(call) = shot::call(snapshot) {
        return Some(call);
    }
    snapshot.call.clone().or_else(|| {
        // A ring that waited beside a call is all that is left once that call ends.
        snapshot
            .ring
            .clone()
            .filter(|ring| matches!(ring.state, CallState::RingingIn { .. }))
    })
}

/// The incoming call an answer/decline shortcut acts on, and whether leaving a call needs confirmation.
fn incoming_action(snapshot: &Snapshot) -> Option<(&Call, bool)> {
    if let Some(ring) = snapshot.ring.as_ref().filter(|ring| matches!(ring.state, CallState::RingingIn { .. })) {
        return Some((ring, snapshot.call.is_some()));
    }
    snapshot.call.as_ref().filter(|call| matches!(call.state, CallState::RingingIn { .. }))
        .map(|call| (call, false))
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
            protocol::launch::no_console(&mut command).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
            if let Ok(out) = out {
                command.stderr(out);
            }
            if let Err(err) = command.spawn() {
                tracing::warn!("tinyclient: could not start chatd: {err}");
            }
        });
    });
}

/// Joined, or was: the only states drawn as "in a call".
fn in_call(call: &Call) -> bool {
    matches!(call.state, CallState::Joined | CallState::Reconnecting)
}

/// The connection is up but the call's keys are not: nothing anyone says is sent or heard yet,
/// so it is not drawn as "connected". Alone in the call there is nobody to wait on.
pub(super) fn securing(call: &Call) -> bool {
    call.state == CallState::Joined
        && !call.participants.is_empty()
        && call.security.as_ref().is_some_and(|security| security.mode == protocol::SecurityMode::Pending)
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
        Network::Telegram => "Open chat in Telegram".to_owned(),
        Network::Messages => "Call in phone app".to_owned(),
        other => format!("Open in {}", other.label()),
    }
}

/// The header pill's label and icon for a hand-off call. Slack shows a headphones glyph and
/// "Huddle" so it reads as a call rather than an unlabelled logo.
fn handoff_pill_parts(network: Network) -> (Option<&'static str>, String) {
    match network {
        Network::Slack => (Some("icons/headphones.svg"), "Huddle".to_owned()),
        other => (None, handoff_label(other)),
    }
}

/// Confirmation toast shown after handing a call off to the network's own app.
fn handoff_toast(network: Network) -> String {
    match network {
        Network::Messages => "Opened in phone app".to_owned(),
        other => format!("Opened in {}", other.label()),
    }
}

fn handoff_tooltip(network: Network) -> String {
    match network {
        Network::Slack => "Open huddle in Slack".to_owned(),
        Network::Telegram => "Opens this chat in the Telegram app to start a call there. tinyclient doesn't carry Telegram calls.".to_owned(),
        Network::Messages => "Opens a phone link in your system's phone app. tinyclient doesn't carry phone calls.".to_owned(),
        other => format!("{0} calls open in the {0} app. tinyclient doesn't carry them.", other.label()),
    }
}

/// Only something with a scheme, never an option.
fn checked_link(url: &str) -> Result<(), String> {
    if url.starts_with('-') || !url.contains(':') {
        return Err(format!("Not a link: {url}"));
    }
    Ok(())
}

/// Hands a link to the system. Only something with a scheme, never an option. Not used on
/// phones (`links_through_gpui`): there the caller opens it on the UI thread with `cx.open_url`.
pub(in crate::gui) fn open(url: &str) -> Result<(), String> {
    checked_link(url)?;
    system_open(url)
}

#[cfg(not(windows))]
fn system_open(url: &str) -> Result<(), String> {
    match std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(url).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(crate::host::nothing_opens(url)),
        Err(_) => Err(crate::host::COULD_NOT_OPEN.to_owned()),
    }
}

/// ShellExecuteW, not `cmd /C start`: cmd splits a link at its `&`
/// (`msteams:/l/channel/…?groupId=…&tenantId=…`) and runs the rest as a command.
#[cfg(windows)]
fn system_open(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{ShellExecuteW, SE_ERR_ASSOCINCOMPLETE, SE_ERR_FNF, SE_ERR_NOASSOC, SE_ERR_PNF};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call; the rest may be null.
    let code = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) } as isize;
    // Above 32 a handler took it. These say no handler is registered, or its program is gone.
    let unregistered = [SE_ERR_FNF, SE_ERR_PNF, SE_ERR_ASSOCINCOMPLETE, SE_ERR_NOASSOC];
    match code {
        33.. => Ok(()),
        code if unregistered.iter().any(|&known| known as isize == code) => Err(crate::host::nothing_opens(url)),
        _ => Err(crate::host::COULD_NOT_OPEN.to_owned()),
    }
}

/// The dock, in the shape of the sidebar's slot (`layout::Slots::call_dock`).
pub(super) fn dock(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> Option<AnyElement> {
    // A phone draws every state of a call full-screen, or as the bar over every screen
    // (`phone`), never as a card at the end of the list.
    if super::phone::active() {
        return None;
    }
    let snapshot = shell.snapshot.as_ref()?;
    shell.call_dock(snapshot, cx).map(IntoElement::into_any_element)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CallGoTarget {
    TinyRoom(protocol::AccountKey, String),
    Conversation,
}

fn call_go_target(key: &Key, is_tiny_room: bool) -> CallGoTarget {
    if key.network == Network::Tiny && is_tiny_room {
        CallGoTarget::TinyRoom(key.account(), key.id.clone())
    } else {
        CallGoTarget::Conversation
    }
}

/// Keyboard action on a participant row in the call card's participant list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKeyAction {
    Profile,
    Menu,
}

fn call_row_key_action(key: &str, shift: bool, other_mod: bool) -> Option<RowKeyAction> {
    if !shift && !other_mod && key == "enter" {
        Some(RowKeyAction::Profile)
    } else if (!other_mod && key == "contextmenu") || (shift && !other_mod && key == "f10") {
        Some(RowKeyAction::Menu)
    } else {
        None
    }
}

/// Number of joined people in a call (me plus every participant who is not still ringing).
pub(super) fn call_people_count(call: &Call) -> usize {
    call.participants.iter().filter(|p| !p.ringing).count() + 1
}

/// The call's popover menu for a participant: volume slider, local mute, Message, Profile, and
/// room moderation when the user has permission on a Tiny server.
impl Shell {
    /// The speaker chosen (V2), in the call's menu or in Settings, which a voice message plays
    /// through too. `None` is the automatic choice.
    pub(super) fn chosen_output(&self) -> Option<String> {
        super::app::prefs::values().device(protocol::settings::OUTPUT)
    }

    /// Offered every change chatd reports, after `Shell` has applied its own part.
    pub(super) fn call_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        ensure_keys(cx);
        if let Change::CallRating { prompt } = change {
            // Never interrupt an active call with a rating card.
            if self.snapshot.as_ref().and_then(|s| s.call.as_ref()).is_none() {
                self.call_open_rating(prompt.clone(), false, cx);
            }
            return;
        }
        let Change::Call { call, .. } = change else {
            return;
        };
        let last = call.as_ref().map(|call| (call.conversation.clone(), discriminant(&call.state)));
        if self.call_state.last != last {
            self.call_state.phone.state_changed(call.as_ref(), self.call_state.last.as_ref().map(|(key, _)| key));
            self.call_state.last = last;
            self.call_state.since = now();
            self.call_state.error = None;
            self.call_reset_watch();
        }
        match call {
            None => {
                self.call_close_popout(cx);
                self.call_menu_set(None, cx);
                let state = &mut self.call_state;
                state.device_notice = None;
                state.mute_before_deafen = None;
                state.confirm = None;
                state.notice = None;
                state.muted_talk = health::MutedTalk::default();
                state.muted_talk_task = None;
                state.metering = None;
                state.cued = None;
                state.auto_watch = None;
                state.discord_speaker = None;
                state.watch_asked = None;
                state.watch_timer = None;
                state.view = None;
                state.focus = None;
                state.full = None;
                state.sharer = None;
                state.share_prompt = None;
                state.watched.clear();
                state.watch_pending = None;
                state.watch_again = None;
                state.watch_sent = false;
                state.watch_epoch = state.watch_epoch.wrapping_add(1);
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
                if self.call_state.connecting.as_ref().is_some_and(|asked| asked.conversation == call.conversation && !matches!(call.state, CallState::Failed { .. })) {
                    self.call_state.connecting = None;
                }
                self.call_state.ended = None;
                self.call_state.rating = None;
                self.call_state.rating_timer = None;
                let menus = matches!(call.state, CallState::RingingOut | CallState::Connecting | CallState::Joined);
                if !menus {
                    self.call_menu_set(None, cx);
                }
                self.call_device_notice(call, cx);
                // Answered with video on a phone: the camera goes on once the call is up.
                if call.state == CallState::Joined && std::mem::take(&mut self.call_state.phone.video_on_join) {
                    self.call_ask(Request::Media(protocol::MediaRequest::Camera { on: true }), cx);
                }
                if !call.deafened {
                    self.call_state.mute_before_deafen = None;
                }
                self.call_watch_muted_talk(call, cx);
                self.call_cue_join(call, cx);
                self.call_share_notice(call, cx);
                self.call_sync_watch(call, cx);
                self.call_auto_watch(call, cx);
                self.call_discord_speaker(call, cx);
                self.call_refresh_popout(cx);
                if let Some((asked_id, _)) = &self.call_state.watch_asked {
                    if call.picture.as_ref().is_some_and(|p| !p.path.is_empty() && p.user_id == *asked_id) {
                        self.call_state.watch_asked = None;
                        self.call_state.watch_timer = None;
                    }
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
                self.call_note_moved(call, &subject, cx);
                let before = self.call_state.was.take().filter(|was| was.conversation == call.conversation);
                // The system moved us to another microphone (the headset was unplugged): said
                // for a few seconds, unless it is the device picked here a moment ago.
                let moved = before.as_ref().is_some_and(|was| {
                    !was.input.is_empty() && was.input != call.input && !call.input.is_empty()
                });
                let picked = super::app::prefs::values().device_choice(protocol::settings::INPUT).name() == Some(call.input.as_str());
                if moved && !picked && call.device_notice.is_none() {
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

    /// A device in use that is not the one chosen, or one that is not working, is said under
    /// the call's title: for a few seconds when something stands in for the chosen device (the
    /// device menu keeps saying it), and until it is fixed when a device stays silent, with the
    /// device menu opened so another can be picked at once.
    fn call_device_notice(&mut self, call: &Call, cx: &mut Context<Self>) {
        if self.call_state.device_notice == call.device_notice {
            return;
        }
        self.call_state.device_notice = call.device_notice.clone();
        match &call.device_notice {
            Some(words) if call.device_stuck => {
                self.call_state.notice = Some(words.clone());
                self.call_state.brief = None;
                if in_call(call) {
                    self.call_menu_set(Some(Menu::Devices), cx);
                }
                cx.notify();
            }
            Some(words) => {
                self.call_state.notice = Some(words.clone());
                self.call_briefly(cx);
            }
            None => {
                self.call_state.notice = None;
                cx.notify();
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

    /// Opens the post-call rating card in the dock (either from `Change::CallRating` or from a
    /// finished call row's Rate button). Untouched prompts auto-dismiss after [`RATING_IDLE`].
    pub(in crate::gui) fn call_open_rating(
        &mut self,
        prompt: protocol::CallRatingPrompt,
        user_initiative: bool,
        cx: &mut Context<Self>,
    ) {
        let title = self
            .snapshot
            .as_ref()
            .and_then(|s| s.open.as_ref())
            .filter(|open| open.key == prompt.conversation)
            .cloned()
            .or_else(|| self.sidebar_state.find(&self.trees, &prompt.conversation))
            .map(|c| c.title)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Call".to_owned());
        let comment_input = cx.new(|cx| super::input::TextInput::new("Optional comment…", cx));
        self.call_state.ended = None;
        self.call_state.rating = Some(CallRatingCard {
            prompt,
            title,
            user_initiative,
            stars: 0,
            reasons: Vec::new(),
            comment_input,
            sending: false,
            error: None,
        });
        self.call_state.rating_timer = if user_initiative {
            None
        } else {
            Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RATING_IDLE).await;
                let _ = this.update(cx, |this, cx| {
                    if this.call_state.rating.as_ref().is_some_and(|r| r.stars == 0 && !r.sending) {
                        this.call_state.rating = None;
                        this.call_state.rating_timer = None;
                        cx.notify();
                    }
                });
            }))
        };
        cx.notify();
    }

    pub(super) fn call_rating_star(&mut self, stars: u8, cx: &mut Context<Self>) {
        if let Some(rating) = self.call_state.rating.as_mut() {
            rating.stars = stars.clamp(1, 5);
            if rating.stars == 5 {
                rating.reasons.clear();
            }
            rating.error = None;
            // Once the person interacts with the card, stop the auto-dismiss timer.
            self.call_state.rating_timer = None;
            cx.notify();
        }
    }

    pub(super) fn call_rating_toggle_reason(&mut self, reason: &'static str, cx: &mut Context<Self>) {
        if let Some(rating) = self.call_state.rating.as_mut() {
            if let Some(pos) = rating.reasons.iter().position(|r| *r == reason) {
                rating.reasons.remove(pos);
            } else {
                rating.reasons.push(reason);
            }
            rating.error = None;
            self.call_state.rating_timer = None;
            cx.notify();
        }
    }

    pub(super) fn call_rating_skip(&mut self, cx: &mut Context<Self>) {
        self.call_state.rating = None;
        self.call_state.rating_timer = None;
        cx.notify();
    }

    pub(super) fn call_rating_send(&mut self, cx: &mut Context<Self>) {
        let Some(rating) = self.call_state.rating.as_mut() else {
            return;
        };
        if rating.stars == 0 || rating.sending {
            return;
        }
        rating.sending = true;
        rating.error = None;
        let comment = rating.comment_input.read(cx).text().to_owned();
        let request = Request::CallRate {
            account: rating.prompt.conversation.account(),
            call_id: rating.prompt.call_id.clone(),
            rating: rating.stars,
            reasons: rating.reasons.iter().map(|r| (*r).to_owned()).collect(),
            comment,
            user_initiative: rating.user_initiative,
        };
        self.call_state.rating_timer = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.call_state.rating = None;
                    cx.notify();
                }
                Ok(Response::Error { detail, .. }) => {
                    if let Some(rating) = this.call_state.rating.as_mut() {
                        rating.sending = false;
                        rating.error = Some(detail);
                    }
                    cx.notify();
                }
                Ok(_) | Err(_) => {
                    if let Some(rating) = this.call_state.rating.as_mut() {
                        rating.sending = false;
                        rating.error = Some("Couldn't send the rating.".to_owned());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A Tiny server moved the call to another room, or Discord moved it to another voice
    /// channel, without my asking: said where to, as a toast that is seen whatever is on
    /// screen, and for a few seconds in the dock.
    fn call_note_moved(&mut self, call: &Call, subject: &Subject, cx: &mut Context<Self>) {
        let Some(was) = self.call_state.was.as_ref() else {
            return;
        };
        let asked = self.call_state.asked.is_some_and(|at| at.elapsed() < MOVE_ASKED);
        if !moved(&was.conversation, &call.conversation) || asked {
            return;
        }
        let words = format!("Moved to {} by a moderator", subject.title);
        self.call_state.notice = Some(words.clone());
        self.call_briefly(cx);
        self.toast(Some(("icons/arrow-right-left.svg", theme::text_soft())), words, cx);
    }

    /// Talking while muted (07 §2a): while I am muted in a call, the meter stays on so the
    /// microphone's level can be heard, and "You're muted" shows when it hears a sentence. The
    /// device menu turns the meter off when it closes, so it is asked for again here.
    fn call_watch_muted_talk(&mut self, call: &Call, cx: &mut Context<Self>) {
        let now = Instant::now();
        let values = super::app::prefs::values();
        let watching = call.state == CallState::Joined && call.muted && !call.deafened && values.on(MUTED_TALK);
        let menu = self.call_state.menu == Some(Menu::Devices);
        if watching && !menu && call.level.is_none() {
            if self.call_state.metering.is_none_or(|at| now.duration_since(at) > METER_AGAIN) {
                self.call_state.metering = Some(now);
                meter(true, cx);
            }
        } else if !watching && !menu && self.call_state.metering.take().is_some() {
            meter(false, cx);
        }
        if self.call_state.muted_talk.feed(watching, call.level, values.tuning().gate, now) {
            // Gone by itself once nothing more is heard.
            self.call_state.muted_talk_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(health::MUTED_TALK_HOLD + Duration::from_millis(50)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            }));
        }
    }

    /// The join cue (S7), when I am first in a call: the same chime others hear when I arrive.
    /// mediad plays it only if Settings › Voice allows join sounds. A move plays its own.
    fn call_cue_join(&mut self, call: &Call, cx: &mut Context<Self>) {
        // A failed call is over: Rejoin is a new arrival, and chimes.
        if matches!(call.state, CallState::Failed { .. }) {
            self.call_state.cued = None;
        }
        if call.state != CallState::Joined || cfg!(feature = "gui-shot") {
            return;
        }
        if self.call_state.cued.replace(call.conversation.clone()).is_none() {
            cx.background_executor()
                .spawn(async move {
                    let _ = ask(&Request::Media(protocol::MediaRequest::Cue { cue: protocol::Cue::Joined }));
                })
                .detach();
        }
    }

    /// "You're muted": I seem to be talking into a muted microphone.
    pub(in crate::gui) fn call_muted_talking(&self, call: &Call) -> bool {
        #[cfg(feature = "gui-shot")]
        if shot::muted_talking() {
            return true;
        }
        call.muted && !call.deafened && self.call_state.muted_talk.showing(Instant::now())
    }

    /// The time on a Tiny stage's banner: how long it has been connecting or reconnecting.
    /// Ticks on its own, apart from the dock's.
    pub(in crate::gui) fn call_stage_clock(&self, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.call_clock(&self.call_state.stage_counter, call, cx)
    }

    /// The dock's ticking text.
    fn call_dock_clock(&self, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.call_clock(&self.call_state.counter, call, cx)
    }

    /// A ticking text for `call`'s state, in the counter `cell` holds.
    fn call_clock(&self, cell: &OnceCell<Entity<Counter>>, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        let counting = match call.state {
            CallState::RingingOut => Counting::Ring,
            CallState::Connecting => Counting::Connect,
            CallState::Joined => Counting::Joined,
            CallState::Reconnecting => Counting::Recover,
            _ => return None,
        };
        let counter = cell.get_or_init(|| cx.new(|_| Counter::default())).clone();
        let what = Some((counting, call.conversation.clone()));
        let joined = call.joined;
        let since = match self.call_state.since {
            0 => now(),
            since => since,
        };
        counter.update(cx, |counter, cx| counter.follow(what, joined, since, cx));
        Some(counter.into_any_element())
    }

    /// A failed call in words, as the dock says it: its title, the line under it, and whether
    /// it can be joined again with one click.
    pub(in crate::gui) fn call_failure(&self, call: &Call) -> Option<(String, String, bool)> {
        let CallState::Failed { reason, detail } = &call.state else {
            return None;
        };
        let subject = Subject::of(self, self.snapshot.as_ref()?, call);
        let failure = dock::failure(call, &subject, *reason, detail);
        let again = failure.acts.iter().any(|act| matches!(act, dock::Act::Again(_)));
        Some((failure.title, failure.line, again))
    }

    /// Goes where the call is: for a Tiny room the server's page on it, else its conversation
    /// with the main-pane call view open over it.
    pub(in crate::gui) fn call_go(&mut self, key: Key, title: &str, cx: &mut Context<Self>) {
        let is_room = key.network == Network::Tiny && self.tiny_room(&key).is_some();
        match call_go_target(&key, is_room) {
            CallGoTarget::TinyRoom(account, room_id) => {
                self.remember();
                self.go(super::sidebar::Mode::Network(Network::Tiny), cx);
                self.prefer_tiny(account, cx);
                self.tiny_pick_channel(Some(room_id), cx);
            }
            CallGoTarget::Conversation => {
                self.select(key.clone(), title, cx);
                self.call_open_view(key, cx);
            }
        }
    }

    /// One person's volume and mute for me on a given account.
    pub(in crate::gui) fn call_gain(&self, account: &AccountKey, user_id: &str) -> (u16, bool) {
        self.call_state.gain(account, user_id)
    }

    /// Sets one person's volume and local mute on `account`. With `send`, chatd persists it and
    /// applies it to the active call (`CallGain`).
    pub(in crate::gui) fn call_set_gain(
        &mut self,
        account: AccountKey,
        user_id: &str,
        percent: u16,
        muted: bool,
        send: bool,
        cx: &mut Context<Self>,
    ) {
        let percent = percent.min(protocol::GAIN_MAX);
        self.call_state.gains.insert((account.clone(), user_id.to_owned()), (percent, muted));
        if send {
            let request = Request::CallGain { account, user_id: user_id.to_owned(), percent, muted };
            self.off_thread(cx, move || ask(&request), |this, answer, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    this.toast(Some(("icons/circle-alert.svg", theme::danger())), detail, cx);
                }
            });
        }
        cx.notify();
    }

    pub(in crate::gui) fn call_slide(&mut self, x: f32, cx: &mut Context<Self>) {
        let (Some((account, user_id)), Some(bounds)) = (self.call_state.sliding.clone(), self.call_state.slider.get()) else {
            return;
        };
        let at = super::components::slider_value(x, f32::from(bounds.origin.x), f32::from(bounds.size.width));
        let mut percent = (at * f32::from(protocol::GAIN_MAX)).round() as u16;
        if percent.abs_diff(100) <= 4 {
            percent = 100;
        }
        let muted = self.call_gain(&account, &user_id).1;
        self.call_set_gain(account, &user_id, percent, muted, false, cx);
    }

    pub(in crate::gui) fn call_slide_end(&mut self, cx: &mut Context<Self>) {
        if let Some((account, user_id)) = self.call_state.sliding.take() {
            let (percent, muted) = self.call_gain(&account, &user_id);
            self.call_set_gain(account, &user_id, percent, muted, true, cx);
        }
    }

    pub(in crate::gui) fn call_track(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.call_state.sliding.is_none() {
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            self.call_slide(f32::from(event.position.x), cx);
        } else {
            self.call_slide_end(cx);
        }
    }

    /// Reusable per-person volume slider and "Mute for me" toggle, shared by the call-owned person
    /// popover, the device menu (for 1:1 calls), and the Tiny server person menu.
    pub(in crate::gui) fn call_volume_controls(&self, account: AccountKey, user_id: &str, cx: &mut Context<Self>) -> Div {
        const SLIDER: f32 = 132.;
        let (percent, muted) = self.call_gain(&account, user_id);
        let slide_account = account.clone();
        let slide_id = user_id.to_owned();
        let bounds = self.call_state.slider.clone();
        let track = div()
            .id(SharedString::from(format!("call-volume-{user_id}")))
            .relative()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.call_state.sliding = Some((slide_account.clone(), slide_id.clone()));
                    this.call_slide(f32::from(event.position.x), cx);
                }),
            )
            .child(slider(f32::from(percent) / f32::from(protocol::GAIN_MAX), SLIDER, Some(0.5)))
            .child(canvas(move |at, _, _| bounds.set(Some(at)), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let mute_account = account;
        let mute_id = user_id.to_owned();
        let mute_icon = if muted { "icons/volume-2.svg" } else { "icons/volume-x.svg" };
        let mute_label = if muted { "Unmute for me" } else { "Mute for me" };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(32.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph(if muted || percent == 0 { "icons/volume-x.svg" } else { "icons/volume-2.svg" }, 16., theme::text_soft()))
                    .child(track)
                    .child(div().flex_1().flex().justify_end().type_small().text_color(theme::text_soft()).child(format!("{percent} %"))),
            )
            .child(
                menu_item(SharedString::from(format!("call-mute-local-{user_id}")), Some(mute_icon), mute_label, None).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let (percent, muted) = this.call_gain(&mute_account, &mute_id);
                    this.call_set_gain(mute_account.clone(), &mute_id, percent, !muted, true, cx);
                })),
            )
    }

    pub(super) fn call_person_menu(&self, account: AccountKey, user_id: &str, name: &str, cx: &mut Context<Self>) -> Div {
        let person_key = Key::new(&account, user_id);
        let msg_person = protocol::Person::new(person_key.clone(), name.to_owned());
        let profile_key = person_key.clone();
        let mut items = div()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.call_track(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.call_slide_end(cx)))
            .child(menu_label(name.to_owned()))
            .child(menu_separator().mx(px(8.)))
            .child(self.call_volume_controls(account.clone(), user_id, cx))
            .child(menu_separator().mx(px(8.)))
            .child(
                menu_item(
                    SharedString::from(format!("call-person-message-{user_id}")),
                    Some("icons/message-square.svg"),
                    "Message",
                    None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_close(cx);
                    this.open_person(&msg_person, cx);
                })),
            )
            .child(
                menu_item(
                    SharedString::from(format!("call-person-profile-{user_id}")),
                    Some("icons/user.svg"),
                    "Profile",
                    None,
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_close(cx);
                    this.open_profile(profile_key.clone(), event.position(), cx);
                })),
            );
        if account.network == Network::Discord {
            if let Some(call) = self.call_now().filter(|call| call.conversation.account() == account) {
                if let (Some(row), Some(participant)) = (self.discord_voice_row(&call.conversation), call.participants.iter().find(|p| p.id == user_id)) {
                    items = items.children(self.discord_voice_actions(&row, &person_key, participant.server_muted, participant.server_deafened, cx));
                }
            }
        }
        if account.network == Network::Tiny {
            if let Some(page) = self.tiny_page_facts().filter(|page| page.account == &account) {
                let facts = super::tiny::model::person_menu(page.community, user_id);
                if facts.server_mute.is_some() || facts.server_deafen.is_some() || facts.disconnect {
                    items = items.child(menu_separator().mx(px(8.))).child(menu_label("Moderation"));
                    if let Some(on) = facts.server_mute {
                        let id = user_id.to_owned();
                        let words = super::tiny::model::server_mute_words(on);
                        items = items.child(
                            menu_item(
                                SharedString::from(format!("call-person-server-mute-{user_id}")),
                                Some(words.icon),
                                words.label,
                                None,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_menu_close(cx);
                                this.tiny_moderate(id.clone(), Some(!on), None, cx);
                            })),
                        );
                    }
                    if let Some(on) = facts.server_deafen {
                        let id = user_id.to_owned();
                        let words = super::tiny::model::server_deafen_words(on);
                        items = items.child(
                            menu_item(
                                SharedString::from(format!("call-person-server-deafen-{user_id}")),
                                Some(words.icon),
                                words.label,
                                None,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_menu_close(cx);
                                this.tiny_moderate(id.clone(), None, Some(!on), cx);
                            })),
                        );
                    }
                    if facts.disconnect {
                        let id = user_id.to_owned();
                        items = items.child(
                            menu_item(
                                SharedString::from(format!("call-person-disconnect-{user_id}")),
                                Some("icons/phone-off.svg"),
                                "Disconnect from call",
                                None,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_menu_close(cx);
                                this.tiny_disconnect(id.clone(), cx);
                            })),
                        );
                    }
                }
            }
        }
        items
    }

    /// Picture path for a call participant: their avatar reference when present (queued once
    /// through the people hub's media path), falling back to what the people hub already knows
    /// about that id (for partial voice states on large servers).
    pub(super) fn call_participant_picture(
        &self,
        account: &protocol::AccountKey,
        who: &protocol::Participant,
        cx: &mut Context<Self>,
    ) -> Option<std::path::PathBuf> {
        if let Some(file) = who.avatar.as_ref() {
            if let Some(path) = self.people_state.hub().file(account, file, cx).path {
                return Some(std::path::PathBuf::from(path));
            }
        }
        let key = Key::new(account, &who.id);
        self.person_picture(&key, cx)
    }

    pub(super) fn call_people_list(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let account = call.conversation.account();
        let mut people: Vec<&protocol::Participant> = call.participants.iter().collect();
        people.sort_by_key(|who| (who.ringing, who.name.to_lowercase()));
        let joined_count = call_people_count(call);
        let noun = if joined_count == 1 { "PERSON" } else { "PEOPLE" };
        let own = super::components::Avatar::named("You", 22.).build();
        let heard = call.speaking && !call.muted && !call.deafened;
        let mut rows = Vec::with_capacity(people.len() + 1);
        rows.push(
            div()
                .w_full()
                .h(px(32.))
                .flex_none()
                .pl(px(12.))
                .pr(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(13.))
                .text_color(if heard { theme::text() } else { theme::text_soft() })
                .child(ringed(own, 22., heard))
                .child(div().flex_1().min_w_0().truncate().child("You"))
                .into_any_element(),
        );
        for who in people {
            let key = Key { network: call.conversation.network, account: call.conversation.account.clone(), id: who.id.clone() };
            let face = self.call_participant_picture(&account, who, cx);
            let avatar = super::components::Avatar::new(&key, &who.name, 22.).image_at(face).build();
            if who.ringing {
                let stop_id = who.id.clone();
                rows.push(
                    div()
                        .id(SharedString::from(format!("call-ringing-row-{}", who.id)))
                        .w_full()
                        .h(px(32.))
                        .flex_none()
                        .pl(px(12.))
                        .pr(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(theme::text_faint())
                        .child(ringed(avatar, 22., false))
                        .child(div().flex_1().min_w_0().truncate().child(who.name.clone()))
                        .child(div().flex_none().type_micro().text_color(theme::text_faint()).child("Ringing…"))
                        .child(
                            div()
                                .id(SharedString::from(format!("call-stop-ring-{}", who.id)))
                                .flex_none()
                                .px(px(6.))
                                .h(px(20.))
                                .rounded(px(6.))
                                .bg(theme::hover())
                                .hover(|s| s.bg(theme::danger_soft()).text_color(theme::danger()))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .type_micro()
                                .text_color(theme::text_soft())
                                .tooltip(tip("Stop ringing", None))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.call_ask(
                                        Request::CallStopRinging { people: Some(vec![stop_id.clone()]) },
                                        cx,
                                    );
                                }))
                                .child("Stop"),
                        )
                        .into_any_element(),
                );
                continue;
            }
            let (percent, muted) = self.call_gain(&account, &who.id);
            let gain_tag = if muted {
                Some("Muted for you".to_owned())
            } else if percent != 100 {
                Some(format!("{percent} %"))
            } else {
                None
            };
            let menu_account = account.clone();
            let menu_id = who.id.clone();
            let menu_name = who.name.clone();
            let open_menu = move |this: &mut Shell, cx: &mut Context<Shell>| {
                cx.stop_propagation();
                this.call_menu_set(Some(Menu::Person { account: menu_account.clone(), user_id: menu_id.clone(), name: menu_name.clone() }), cx);
            };
            let open_menu_more = open_menu.clone();
            let open_menu_key = open_menu.clone();
            let profile_key = key.clone();
            let key_enter = key.clone();
            let row_group: SharedString = format!("call-person-row-{}", who.id).into();
            let watch_btn = who.sharing.then(|| {
                let watch_id = who.id.clone();
                let watch_call = call.clone();
                let watch_title = subject.title.clone();
                div()
                    .id(SharedString::from(format!("call-person-watch-{}", who.id)))
                    .flex_none()
                    .size(px(22.))
                    .rounded(px(5.))
                    .bg(theme::accent_soft())
                    .hover(|s| s.bg(theme::accent_wash()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tooltip(tip(format!("Watch {}'s screen", who.name), None))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_menu_close(cx);
                        this.call_watch_stream(&watch_call, watch_id.clone(), &watch_title, cx);
                    }))
                    .child(glyph("icons/monitor-up.svg", 13., theme::accent()))
            });
            let camera_glyph = who.camera.then(|| {
                div()
                    .id(SharedString::from(format!("call-person-camera-{}", who.id)))
                    .flex_none()
                    .flex()
                    .items_center()
                    .tooltip(tip("Camera on", None))
                    .child(glyph("icons/video.svg", 13., theme::text_soft()))
            });
            let more_btn = div()
                .id(SharedString::from(format!("call-person-more-{}", who.id)))
                .flex_none()
                .size(px(22.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.)
                .group_hover(row_group.clone(), |s| s.opacity(1.))
                .hover(|s| s.bg(theme::selected()))
                .cursor_pointer()
                .tooltip(tip("Volume and more", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| open_menu_more(this, cx)))
                .child(glyph("icons/ellipsis.svg", 13., theme::text_soft()));
            rows.push(
                div()
                    .id(row_group.clone())
                    .group(row_group)
                    .w_full()
                    .h(px(32.))
                    .flex_none()
                    .pl(px(12.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .tab_index(0)
                    .focus_visible(|style| style.shadow(theme::focus_ring()))
                    .hover(|s| s.bg(theme::hover()))
                    .text_size(px(13.))
                    .text_color(if who.speaking { theme::text() } else { theme::text_soft() })
                    .tooltip(tip("Open profile", None))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.call_menu_close(cx);
                        this.open_profile(profile_key.clone(), event.position(), cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| open_menu(this, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        let mods = event.keystroke.modifiers;
                        let other = mods.control || mods.alt || mods.platform || mods.function;
                        match call_row_key_action(event.keystroke.key.as_str(), mods.shift, other) {
                            Some(RowKeyAction::Profile) => {
                                cx.stop_propagation();
                                this.call_menu_close(cx);
                                this.show_info(key_enter.clone(), cx);
                            }
                            Some(RowKeyAction::Menu) => open_menu_key(this, cx),
                            None => {}
                        }
                    }))
                    .child(ringed(avatar, 22., who.speaking))
                    .child(div().flex_1().min_w_0().truncate().child(who.name.clone()))
                    .children(gain_tag.map(|tag| div().flex_none().type_micro().text_color(if muted { theme::warning() } else { theme::text_faint() }).child(tag)))
                    .children(watch_btn)
                    .children(camera_glyph)
                    .when(who.server_muted, |r| r.child(glyph("icons/mic-off.svg", 13., theme::danger())))
                    .when(!who.server_muted && who.suppressed, |r| r.child(glyph("icons/mic-off.svg", 13., theme::warning())))
                    .when(!who.server_muted && !who.suppressed && who.muted && !who.deafened && !who.server_deafened, |r| {
                        r.child(glyph("icons/mic-off.svg", 13., theme::text_faint()))
                    })
                    .when(who.server_deafened, |r| r.child(glyph("icons/headphone-off.svg", 13., theme::danger())))
                    .when(!who.server_deafened && who.deafened, |r| r.child(glyph("icons/headphone-off.svg", 13., theme::text_faint())))
                    .child(more_btn)
                    .into_any_element(),
            );
        }
        let can_ring_group = call.conversation.network == Network::Discord && subject.kind == ConversationKind::Group;
        let can_add_people = call.conversation.network == Network::Discord
            && subject.kind.is_direct()
            && (call.participants.len() + 1) < 10;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(4.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_faint())
                    .child(format!("{} · {joined_count} {noun}", subject.title.to_uppercase()))
                    .when(can_ring_group || can_add_people, |header| {
                        header.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .when(can_add_people, |actions| {
                                    actions.child(
                                        div()
                                            .id("call-add-people-open")
                                            .px(px(6.))
                                            .h(px(20.))
                                            .rounded(px(6.))
                                            .bg(theme::hover())
                                            .hover(|s| s.bg(theme::selected()).text_color(theme::text()))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .type_micro()
                                            .text_color(theme::text_soft())
                                            .tooltip(tip("Add friends to call", None))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.call_menu_set(Some(Menu::AddPeople), cx);
                                            }))
                                            .child("Add people"),
                                    )
                                })
                                .when(can_ring_group, |actions| {
                                    actions.child(
                                        div()
                                            .id("call-ring-group")
                                            .px(px(6.))
                                            .h(px(20.))
                                            .rounded(px(6.))
                                            .bg(theme::hover())
                                            .hover(|s| s.bg(theme::selected()).text_color(theme::text()))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .type_micro()
                                            .text_color(theme::text_soft())
                                            .tooltip(tip("Ring everyone in the group", None))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.call_ask(Request::CallRing { people: None }, cx);
                                            }))
                                            .child("Ring"),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .id("call-people-rows")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }

    /// Ask mediad to watch `user_id`'s stream and schedule 3 s / 10 s redraws so the tile never
    /// stays on "Connecting…" silently. On Tiny the subscription is `call.watch`, not ShareWatch.
    pub(in crate::gui) fn call_watch(&mut self, user_id: String, cx: &mut Context<Self>) {
        self.call_state.share_prompt = None;
        self.call_state.watch_asked = Some((user_id.clone(), Instant::now()));
        self.call_state.watch_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
            cx.background_executor().timer(Duration::from_secs(7)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        if self.call_now().is_some_and(|call| call.conversation.network == Network::Tiny) {
            return;
        }
        self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source: Some(user_id) }), cx);
    }

    /// One-click watch from a picture box or a phone tile. Tiny subscribes with `call.watch`.
    pub(in crate::gui) fn call_watch_user(&mut self, user_id: String, cx: &mut Context<Self>) {
        if self.call_now().is_some_and(|call| call.conversation.network == Network::Tiny) {
            self.call_state.focus = Some(stage::Focus::Stream(user_id.clone()));
            self.call_watch(user_id, cx);
            self.call_sync_watch_now(cx);
            return;
        }
        self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source: Some(user_id) }), cx);
    }

    /// Stop the picture-box or phone watch. On Tiny that is a focus change and `call.watch`.
    pub(in crate::gui) fn call_stop_watch_user(&mut self, cx: &mut Context<Self>) {
        if self.call_now().is_some_and(|call| call.conversation.network == Network::Tiny) {
            if matches!(self.call_state.focus, Some(stage::Focus::Stream(_))) {
                self.call_state.focus = None;
            }
            self.call_unwatch(cx);
            self.call_sync_watch_now(cx);
            return;
        }
        self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source: None }), cx);
    }

    /// Watch `user_id`'s screen, focus their stream on the stage, dismiss any share notice, and
    /// open the call view or room stage.
    pub(in crate::gui) fn call_watch_stream(
        &mut self,
        call: &Call,
        user_id: String,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        self.call_state.share_prompt = None;
        self.call_state.focus = Some(stage::Focus::Stream(user_id.clone()));
        if !watching(call, &user_id) {
            self.call_watch(user_id, cx);
        }
        self.call_go(call.conversation.clone(), title, cx);
        self.call_sync_watch_now(cx);
    }

    /// Dismiss the dock's screen-share notice strip until the sharer stops and someone shares again.
    pub(super) fn call_dismiss_share_prompt(&mut self, cx: &mut Context<Self>) {
        self.call_state.share_prompt = None;
        cx.notify();
    }

    /// True when the user is currently looking at `call`'s main-pane call view or Tiny room stage.
    fn call_looking_at(&self, call: &Call) -> bool {
        if self.call_state.view.as_ref() == Some(&call.conversation) {
            return true;
        }
        call.conversation.network == Network::Tiny
            && self.tiny_open()
            && self.tiny_account() == Some(&call.conversation.account())
            && self.tiny_state.channel.as_deref() == Some(call.conversation.id.as_str())
    }

    /// Notices when a remote participant starts sharing their screen: plays a soft cue, offers
    /// Watch on the dock card, and optionally auto-watches when the user is already looking at the
    /// call.
    fn call_share_notice(&mut self, call: &Call, cx: &mut Context<Self>) {
        let started = share_notice_step(call, &mut self.call_state.sharer, &mut self.call_state.share_prompt);
        if self.call_state.share_prompt.as_deref().is_some_and(|id| watching(call, id)) {
            self.call_state.share_prompt = None;
        }
        let Some(id) = started else {
            return;
        };
        if !cfg!(feature = "gui-shot") && !super::streamer::is_active() {
            cx.background_executor()
                .spawn(async move {
                    let _ = ask(&Request::Media(protocol::MediaRequest::Cue {
                        cue: protocol::Cue::ShareStarted,
                    }));
                })
                .detach();
        }
        let auto_watch = super::app::prefs::values().on(protocol::settings::AUTO_WATCH_SHARE);
        if should_auto_watch_share(auto_watch, self.call_looking_at(call), self.call_state.focus.is_some()) {
            self.call_state.share_prompt = None;
            self.call_state.focus = Some(stage::Focus::Stream(id.clone()));
            if !watching(call, &id) {
                self.call_watch(id, cx);
            }
        }
    }

    /// Reset the decoder and ask for a fresh keyframe for `user_id`.
    pub(in crate::gui) fn call_retry_watch(&mut self, user_id: String, cx: &mut Context<Self>) {
        if self.call_now().is_some_and(|call| call.conversation.network == Network::Tiny) {
            self.call_state.watched.clear();
            self.call_state.watch_pending = None;
            self.call_state.watch_again = None;
            self.call_state.watch_epoch = self.call_state.watch_epoch.wrapping_add(1);
            self.call_state.focus = Some(stage::Focus::Stream(user_id.clone()));
            self.call_watch(user_id, cx);
            self.call_sync_watch_now(cx);
            return;
        }
        self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source: None }), cx);
        self.call_watch(user_id, cx);
    }

    /// Stop watching the current stream and clear any pending connecting timer.
    pub(in crate::gui) fn call_unwatch(&mut self, cx: &mut Context<Self>) {
        self.call_state.watch_asked = None;
        self.call_state.watch_timer = None;
        if self.call_now().is_some_and(|call| share_watch_superseded(call.conversation.network, self.call_state.watch_sent)) {
            return;
        }
        self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source: None }), cx);
    }

    /// In a 1:1 Tiny direct call, automatically watch the peer's screen share or camera as soon
    /// as they start sending, and stop decoding when they stop. Once `call.watch` is the
    /// subscription, ShareWatch would replace that list with one share.
    fn call_auto_watch(&mut self, call: &Call, cx: &mut Context<Self>) {
        let is_room = self.tiny_room(&call.conversation).is_some();
        if let Some(source) = auto_watch_step(call, is_room, &mut self.call_state.auto_watch) {
            if share_watch_superseded(call.conversation.network, self.call_state.watch_sent) {
                return;
            }
            self.call_ask(Request::Media(protocol::MediaRequest::ShareWatch { source }), cx);
        }
    }

    /// For a joined Discord call, tell mediad whose camera to paint (`MediaRequest::Speaker`),
    /// debouncing active-speaker switches by 800 ms.
    fn call_discord_speaker(&mut self, call: &Call, cx: &mut Context<Self>) {
        let now = Instant::now();
        let current = self.call_state.discord_speaker.as_ref().map(|(id, at)| (id.as_str(), *at));
        if let Some(next) = discord_speaker_step(call, None, current, now) {
            self.call_state.discord_speaker = next.clone().map(|id| (id, now));
            self.call_ask(Request::Media(protocol::MediaRequest::Speaker { source: next }), cx);
        }
    }

    /// Drops the watch bookkeeping so the next sync subscribes again. A late answer sees the
    /// new epoch and does not apply.
    fn call_reset_watch(&mut self) {
        let state = &mut self.call_state;
        state.watched.clear();
        state.watch_pending = None;
        state.watch_again = None;
        state.watch_sent = false;
        state.watch_epoch = state.watch_epoch.wrapping_add(1);
    }

    /// Synchronizes multi-stream `MediaRequest::Watch` when the joined Tiny call's visible
    /// cameras or focused stream change. The list counts only after chatd says ok. A refusal
    /// is shown and asked again, but not on every picture frame.
    pub(in crate::gui) fn call_sync_watch(&mut self, call: &Call, cx: &mut Context<Self>) {
        if call.conversation.network != Network::Tiny {
            return;
        }
        let budget_small = if super::phone::active() { 2 } else { 4 };
        let next = desired_watch_sources(call, self.call_state.focus.as_ref(), budget_small);
        if self.call_state.watch_pending.as_ref() == Some(&next) || self.call_state.watched == next {
            return;
        }
        if self.call_state.watch_again.as_ref().is_some_and(|(sources, until)| sources == &next && Instant::now() < *until) {
            return;
        }
        self.call_state.watch_epoch = self.call_state.watch_epoch.wrapping_add(1);
        let epoch = self.call_state.watch_epoch;
        self.call_state.watch_pending = Some(next.clone());
        self.call_state.watch_sent = true;
        let request = Request::Media(protocol::MediaRequest::Watch { sources: next.clone() });
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.call_state.watch_epoch != epoch {
                    return;
                }
                this.call_state.watch_pending = None;
                let detail = match &answer {
                    Ok(Response::Error { detail, .. }) => Some(detail.clone()),
                    Err(err) => Some(crate::client::trouble_words("call", err)),
                    Ok(_) => None,
                };
                if let Some(detail) = detail {
                    this.call_state.watch_again = Some((next, Instant::now() + WATCH_AGAIN));
                    this.call_state.error = Some(detail.clone());
                    this.refused = Some(detail);
                } else {
                    this.call_state.watched = next;
                    this.call_state.watch_again = None;
                    this.call_state.error = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-evaluates multi-stream watch subscriptions for the currently active call (e.g. after focus changes).
    /// A click or a focus change retries a refused list immediately.
    pub(in crate::gui) fn call_sync_watch_now(&mut self, cx: &mut Context<Self>) {
        self.call_state.watch_again = None;
        if let Some(call) = self.snapshot.as_ref().and_then(|s| s.call.clone()) {
            self.call_sync_watch(&call, cx);
        }
    }

    /// The received video frame on screen, when it belongs to `(user_id, kind)`.
    pub(in crate::gui) fn call_frame(
        &self,
        call: Option<&Call>,
        user_id: &str,
        kind: protocol::PictureKind,
    ) -> Option<std::sync::Arc<gpui::RenderImage>> {
        let call = call?;
        if call
            .pictures
            .iter()
            .any(|p| !p.path.is_empty() && p.user_id == user_id && p.kind == kind)
        {
            if let Some(img) = self
                .call_state
                .feeds
                .get(&(user_id.to_owned(), kind))
                .and_then(|feed| feed.shown().cloned())
            {
                return Some(img);
            }
        }
        call.picture
            .as_ref()
            .filter(|picture| !picture.path.is_empty() && picture.user_id == user_id && picture.kind == kind)?;
        self.call_state.feed.shown().cloned()
    }

    /// My own camera, or with `share` my own screen, as it is sent, while it is on.
    pub(in crate::gui) fn call_self_frame(
        &self,
        call: Option<&Call>,
        share: bool,
    ) -> Option<std::sync::Arc<gpui::RenderImage>> {
        let (picture, feed) = if share {
            (call?.share_view.as_ref(), &self.call_state.screen_feed)
        } else {
            (call?.self_view.as_ref(), &self.call_state.self_feed)
        };
        picture.filter(|picture| !picture.path.is_empty())?;
        feed.shown().cloned()
    }

    /// Video picture area shown above the messages when the open conversation is a non-room call
    /// carrying a peer picture or local self-view.
    pub(in crate::gui) fn call_conversation_picture(
        &self,
        snapshot: &Snapshot,
        open: &protocol::Conversation,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let call = self.call_shown(snapshot).filter(|call| {
            call.state == CallState::Joined
                && call.conversation == open.key
                && self.tiny_room(&call.conversation).is_none()
        })?;
        self.call_picture_box(&call, false, cx)
    }

    /// Compact picture tile shown above the dock card when a non-room call has live video while
    /// another conversation is open.
    pub(super) fn call_dock_picture(
        &self,
        snapshot: &Snapshot,
        call: &Call,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if call.state != CallState::Joined || self.tiny_room(&call.conversation).is_some() {
            return None;
        }
        if snapshot.open.as_ref().is_some_and(|open| open.key == call.conversation) {
            return None;
        }
        self.call_picture_box(call, true, cx)
    }

    fn call_picture_box(&self, call: &Call, compact: bool, cx: &mut Context<Self>) -> Option<Div> {
        let peer_picture = call.picture.as_ref().filter(|p| !p.path.is_empty());
        let peer_sender = call.participants.iter().find(|p| p.sharing || p.camera);
        let peer_id = peer_picture
            .map(|p| p.user_id.as_str())
            .or_else(|| peer_sender.map(|p| p.id.as_str()));
        let peer_name = peer_id
            .and_then(|id| call.participants.iter().find(|p| p.id == id))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Peer".to_owned());
        let peer_is_share = peer_picture
            .map(|p| p.kind == protocol::PictureKind::Share)
            .or_else(|| peer_sender.map(|p| p.sharing))
            .unwrap_or(false);
        let peer_kind = if peer_is_share { protocol::PictureKind::Share } else { protocol::PictureKind::Camera };
        let peer_frame = peer_id.and_then(|id| self.call_frame(Some(call), id, peer_kind));
        let my_share = self.call_self_frame(Some(call), true);
        let my_camera = self.call_self_frame(Some(call), false);
        let self_frame = my_share.clone().or_else(|| my_camera.clone());
        let self_is_share = my_share.is_some();

        let has_peer = peer_frame.is_some() || peer_sender.is_some();
        if !has_peer && self_frame.is_none() {
            return None;
        }
        let height = if compact { 126. } else { 260. };
        if self.call_popout_open() {
            let placeholder = div()
                .relative()
                .w_full()
                .h(px(if compact { 64. } else { 112. }))
                .flex_none()
                .rounded(px(if compact { theme::R_CARD } else { 10. }))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::hairline())
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .type_small()
                        .text_color(theme::text_soft())
                        .child(glyph("icons/picture-in-picture-2.svg", 14., theme::accent()))
                        .child("Playing in a separate window"),
                )
                .child(
                    div()
                        .id(if compact { "call-picture-bring-back-compact" } else { "call-picture-bring-back" })
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .bg(theme::selected())
                        .text_color(theme::text())
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::pressed()))
                        .type_micro()
                        .font_weight(theme::SEMIBOLD)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_close_popout(cx);
                        }))
                        .child("Bring back"),
                );
            return if compact {
                Some(div().w_full().pb(px(6.)).child(placeholder))
            } else {
                Some(div().w_full().flex_none().px(px(16.)).pt(px(10.)).pb(px(4.)).child(placeholder))
            };
        }
        let main_is_peer = has_peer;

        let main_body = if main_is_peer {
            if let Some(frame) = peer_frame {
                gpui::img(frame).size_full().object_fit(gpui::ObjectFit::Contain).into_any_element()
            } else {
                let id = peer_id.unwrap_or_default().to_owned();
                let is_watching = watching(call, &id);
                let label = if is_watching { "Connecting…" } else if peer_is_share { "Watch stream" } else { "Watch camera" };
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(div().type_small().text_color(theme::text_soft()).child(format!(
                        "{peer_name} is {}",
                        if peer_is_share { "sharing their screen" } else { "on camera" }
                    )))
                    .child(
                        div()
                            .id("call-picture-watch")
                            .h(px(28.))
                            .px(px(12.))
                            .rounded_full()
                            .bg(if is_watching { theme::overlay() } else { theme::accent() })
                            .text_color(if is_watching { theme::text_soft() } else { theme::on_accent() })
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .type_micro()
                            .font_weight(theme::SEMIBOLD)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_watch_user(id.clone(), cx);
                            }))
                            .child(label),
                    )
                    .into_any_element()
            }
        } else {
            gpui::img(self_frame.clone().unwrap())
                .size_full()
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element()
        };

        let caption = if main_is_peer {
            format!("{peer_name}'s {}", if peer_is_share { "screen" } else { "camera" })
        } else if self_is_share {
            "Your screen".to_owned()
        } else {
            "Your camera".to_owned()
        };
        let badge = (main_is_peer && peer_is_share).then(|| {
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(theme::danger())
                .text_color(theme::on_accent())
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .child("LIVE")
        });
        let action_btn = if main_is_peer && peer_id.is_some_and(|id| watching(call, id)) {
            Some(
                div()
                    .id("call-picture-stop-watch")
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(theme::overlay())
                    .text_color(theme::text())
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .type_micro()
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_stop_watch_user(cx);
                    }))
                    .child("Stop watching"),
            )
        } else if !main_is_peer {
            Some(
                div()
                    .id("call-picture-stop-self")
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(theme::danger())
                    .text_color(theme::on_accent())
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .type_micro()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if self_is_share {
                            this.tiny_stop_share(cx);
                        } else {
                            this.call_ask(Request::Media(protocol::MediaRequest::Camera { on: false }), cx);
                        }
                    }))
                    .child("Stop"),
            )
        } else {
            None
        };
        let pip = (main_is_peer && !compact).then(|| self_frame).flatten().map(|frame| {
            div()
                .absolute()
                .bottom(px(8.))
                .right(px(8.))
                .w(px(112.))
                .h(px(64.))
                .rounded(px(8.))
                .overflow_hidden()
                .border_1()
                .border_color(theme::border())
                .bg(gpui::rgb(0x0b0b0d))
                .child(gpui::img(frame).size_full().object_fit(gpui::ObjectFit::Contain))
        });

        let box_div = div()
            .relative()
            .w_full()
            .h(px(height))
            .flex_none()
            .rounded(px(if compact { theme::R_CARD } else { 10. }))
            .bg(gpui::rgb(0x0b0b0d))
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .child(main_body)
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .bg(theme::overlay())
                    .children(badge)
                    .child(div().type_micro().text_color(theme::text()).child(caption)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id(if compact { "call-picture-popout-compact" } else { "call-picture-popout" })
                            .size(px(24.))
                            .rounded(px(6.))
                            .bg(theme::overlay())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::pressed()))
                            .tooltip(tip("Pop out video", Some("⇧⌘O")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_toggle_popout(cx);
                            }))
                            .child(glyph("icons/picture-in-picture-2.svg", 13., theme::text())),
                    )
                    .children(action_btn),
            )
            .children(pip);

        if compact {
            Some(div().w_full().pb(px(6.)).child(box_div))
        } else {
            Some(div().w_full().flex_none().px(px(16.)).pt(px(10.)).pb(px(4.)).child(box_div))
        }
    }

    /// True while the call's video is playing in its own always-on-top pop-out window (`#267`).
    pub(in crate::gui) fn call_popout_open(&self) -> bool {
        self.call_state.popout.is_some()
    }

    pub(in crate::gui) fn call_open_popout(&mut self, cx: &mut Context<Self>) {
        if crate::gui::phone::active() || self.call_state.popout.is_some() {
            return;
        }
        let active_call = self
            .snapshot
            .as_ref()
            .and_then(|s| self.call_shown(s))
            .is_some_and(|c| in_call(&c));
        if !active_call {
            return;
        }
        let shell = cx.weak_entity();
        let saved = self.call_state.popout_frame;
        cx.defer(move |cx| {
            if let Some(handle) = popout::open(shell.clone(), saved, cx) {
                let _ = shell.update(cx, |this, cx| {
                    this.call_state.popout = Some(handle);
                    cx.notify();
                });
            }
        });
    }

    pub(in crate::gui) fn call_close_popout(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.call_state.popout.take() else {
            return;
        };
        cx.notify();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        });
    }

    pub(in crate::gui) fn call_toggle_popout(&mut self, cx: &mut Context<Self>) {
        if self.call_state.popout.is_some() {
            self.call_close_popout(cx);
        } else {
            self.call_open_popout(cx);
        }
    }

    pub(in crate::gui) fn call_refresh_popout(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.call_state.popout else {
            return;
        };
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, _, cx| cx.notify());
        });
    }

    /// Optional setting (V26): automatically pop out the call video when the main window loses
    /// focus while a remote screen share is being watched.
    pub(in crate::gui) fn call_auto_popout_on_blur(&mut self, main_active: bool, cx: &mut Context<Self>) {
        if crate::gui::phone::active() {
            return;
        }
        let call = self.snapshot.as_ref().and_then(|s| self.call_shown(s));
        if popout::should_auto_popout(
            super::app::prefs::choices().auto_popout_call,
            main_active,
            self.call_state.popout.is_some(),
            call.as_ref(),
        ) {
            self.call_open_popout(cx);
        }
    }

    /// Reads the current call and its already-decoded frames from `call_state` for `CallPopout`.
    pub(super) fn call_popout_snapshot(&self) -> Option<PopoutSnapshot> {
        let snapshot = self.snapshot.as_ref()?;
        let call = self.call_shown(snapshot).filter(|c| in_call(c))?;
        let subject = Subject::of(self, snapshot, &call);
        let focused = match &self.call_state.focus {
            Some(stage::Focus::Stream(id)) => Some(id.as_str()),
            _ => None,
        };
        let owner = popout::preferred_frame_owner(&call, focused);
        let remote_frame = owner.as_ref().and_then(|(id, share)| {
            let kind = if *share { protocol::PictureKind::Share } else { protocol::PictureKind::Camera };
            self.call_frame(Some(&call), id, kind)
        });
        let my_share = self.call_self_frame(Some(&call), true);
        let my_camera = self.call_self_frame(Some(&call), false);
        let local_frame = my_share.clone().or(my_camera);

        let (main_frame, pip_frame, live_badge, title) = if let Some(frame) = remote_frame {
            let (owner_id, is_share) = owner.as_ref().map(|(id, s)| (id.as_str(), *s)).unwrap_or(("", false));
            let name = call
                .participants
                .iter()
                .find(|p| p.id == owner_id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| subject.title.clone());
            let label = format!("{name}'s {}", if is_share { "screen" } else { "camera" });
            (Some(frame), local_frame, is_share, label)
        } else if let Some(frame) = local_frame {
            let is_share = my_share.is_some();
            let label = if is_share { "Your screen".to_owned() } else { "Your camera".to_owned() };
            (Some(frame), None, is_share, label)
        } else {
            (None, None, false, subject.title.clone())
        };

        let speaker = owner
            .as_ref()
            .and_then(|(id, _)| call.participants.iter().find(|p| p.id == *id));
        let speaker_name = speaker
            .map(|p| p.name.clone())
            .unwrap_or_else(|| subject.title.clone());
        let speaker_speaking = speaker.is_some_and(|p| p.speaking);
        let camera_on = call.self_view.is_some()
            || call.video.iter().any(|v| v.direction == protocol::Direction::Encode && v.content == protocol::Content::Camera);
        let share_on = call.share_view.is_some()
            || call.video.iter().any(|v| v.direction == protocol::Direction::Encode && v.content == protocol::Content::Share);
        let can_pictures = call.state == CallState::Joined && protocol::call_pictures(call.conversation.network);

        Some(PopoutSnapshot {
            title,
            muted: call.muted || call.deafened,
            camera_on,
            share_on,
            can_pictures,
            main_frame,
            pip_frame,
            speaker_name,
            speaker_speaking,
            live_badge,
        })
    }

    /// A join was asked for here: a new room that follows is not a moderator's move.
    pub(in crate::gui) fn call_asked(&mut self) {
        self.call_state.asked = Some(Instant::now());
    }

    /// Friend picker popover for adding friends to a Discord 1:1 or group DM call.
    /// Filtered to `Relation::Friend` on the call's account, excluding people already in the call,
    /// and respecting Discord's 10-person group DM ceiling.
    pub(super) fn call_add_people_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        const LIMIT: usize = 10;
        let account = call.conversation.account();
        let me = self
            .snapshot
            .as_ref()
            .and_then(|snap| snap.accounts.iter().find(|a| a.key == account))
            .map(|a| a.me.clone())
            .unwrap_or_default();
        let in_call_ids: std::collections::HashSet<&str> = call.participants.iter().map(|p| p.id.as_str()).collect();
        let seats_left = LIMIT.saturating_sub(call.participants.len() + 1);
        let candidates: Vec<protocol::Person> = self
            .people_state
            .hub()
            .friends(&account)
            .into_iter()
            .filter(|p| p.key.id != me && !in_call_ids.contains(p.key.id.as_str()))
            .collect();
        let rows: Vec<AnyElement> = if seats_left == 0 {
            vec![div()
                .px(px(12.))
                .py(px(8.))
                .type_small()
                .text_color(theme::text_faint())
                .child("This group call is full (10 people).")
                .into_any_element()]
        } else if candidates.is_empty() {
            vec![div()
                .px(px(12.))
                .py(px(8.))
                .type_small()
                .text_color(theme::text_faint())
                .child("No other friends available to add.")
                .into_any_element()]
        } else {
            candidates
                .into_iter()
                .map(|friend| {
                    let person_id = friend.key.id.clone();
                    let face = self.person_picture(&friend.key, cx);
                    let avatar = super::components::Avatar::new(&friend.key, &friend.name, 22.).image_at(face).build();
                    div()
                        .id(SharedString::from(format!("call-add-friend-{person_id}")))
                        .w_full()
                        .h(px(32.))
                        .flex_none()
                        .px(px(10.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .text_size(px(13.))
                        .text_color(theme::text())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_menu_close(cx);
                            this.call_ask(Request::CallAddPeople { people: vec![person_id.clone()] }, cx);
                        }))
                        .child(avatar)
                        .child(div().flex_1().min_w_0().truncate().child(friend.name))
                        .into_any_element()
                })
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_faint())
                    .child("ADD FRIENDS TO CALL")
                    .child(format!("{seats_left} left")),
            )
            .child(
                div()
                    .id("call-add-people-rows")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }

    /// The name of whoever was in this call with me and is not any more.
    pub(super) fn call_left_alone(&self, call: &Call) -> Option<String> {
        if call.state != CallState::Joined || call.participants.iter().any(|p| !p.ringing) {
            return None;
        }
        #[cfg(feature = "gui-shot")]
        if let Some(left) = shot::left() {
            return Some(left);
        }
        let was = self.call_state.was.as_ref().filter(|was| was.conversation == call.conversation)?;
        was.peer.clone()
    }

    /// What the last request about the call was refused with, for the channel stage.
    pub(in crate::gui) fn call_refused(&self) -> Option<&str> {
        self.call_state.error.as_deref()
    }

    /// One request about the call, asked off the main thread. chatd answers the outcome as a
    /// `Change::Call`; only a refusal is kept, to be shown in the dock and by the composer.
    pub(in crate::gui) fn call_ask(&mut self, request: Request, cx: &mut Context<Self>) {
        self.call_state.error = None;
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words("call", &err)),
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
        #[cfg(feature = "gui-shot")]
        if shot::tiny() {
            // A made-up Tiny call is in the room I sit in on the server on show.
            let mut call = shot::call(snapshot)?;
            call.conversation = self.tiny_shot_room()?;
            // Everyone else in the room's call is in its encrypted group, named as chatd names
            // a Tiny call's roster.
            let others = self.tiny_shot_others(&call.conversation.id);
            call.security = shot::security(&others.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>());
            if call.participants.is_empty() {
                call.participants = others.into_iter().map(|(id, name)| protocol::Participant { id, name, ..Default::default() }).collect();
            }
            return Some(call);
        }
        drawn(shown(snapshot), self.call_state.local.as_ref(), self.call_state.connecting.as_ref())
    }

    /// Draws a call just asked for as Connecting before chatd has said a word about it.
    pub(in crate::gui) fn call_connecting_now(&mut self, conversation: Key, (muted, deafened): (bool, bool), cx: &mut Context<Self>) {
        self.call_state.connecting = Some(bare_call(conversation, CallState::Connecting, muted, deafened));
        cx.notify();
    }

    /// The ask for that call was answered: chatd's state is the truth now. An ask that worked
    /// keeps the drawing a moment longer when chatd's call has not reached this window yet, so
    /// the dock does not blink out between the two; one that failed drops it at once.
    pub(in crate::gui) fn call_connecting_over(&mut self, worked: bool, cx: &mut Context<Self>) {
        if connecting_settled(worked, self.call_now()) {
            self.call_state.connecting = None;
            cx.notify();
            return;
        }
        // Only the join this timer was armed for is let go of: a later one has its own.
        let kept = self.call_state.connecting.as_ref().map(|call| call.conversation.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if expire_connecting(&mut this.call_state.connecting, kept.as_ref()) {
                    cx.notify();
                }
            });
        })
        .detach();
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
        if call.server_muted || call.suppressed {
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
        if call.state != CallState::Joined || call.server_deafened {
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
        let asked = state.connecting.take();
        if state.local.take().is_some() | state.ended.take().is_some() | state.confirm.take().is_some() | asked.is_some() {
            cx.notify();
        }
        // A join that chatd has not yet shown is still on its way: cancelling it is the same ask
        // as the Cancel button beside it.
        if asked.is_some_and(|call| call.conversation.network == Network::Tiny) {
            self.tiny_leave(cx);
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
        let Some((ring, confirm)) = self.snapshot.as_ref().and_then(incoming_action) else { return };
        let conversation = ring.conversation.clone();
        if confirm {
            let CallState::RingingIn { from } = &ring.state else { return };
            self.call_state.confirm = Some(Confirm { conversation, title: from.clone(), answer: true });
            cx.notify();
        } else {
            self.call_begin(vec![Request::CallAccept], conversation, cx);
        }
    }

    /// Decline the incoming call, including one waiting beside an active call.
    pub(super) fn call_decline(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.as_ref().and_then(incoming_action).is_some() {
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
        self.call_go(call.conversation, &title, cx);
    }

    /// Rings or joins. A failed call in the dock is replaced by the new one. A call that is up
    /// somewhere else is not left without a word: the dock asks first.
    pub(in crate::gui) fn call_start(&mut self, conversation: Key, cx: &mut Context<Self>) {
        if let Some(call) = self.call_now() {
            if call.conversation == conversation && !matches!(call.state, CallState::Failed { .. })
            {
                return;
            }
        }
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
        self.call_asked();
        // Answering a ring starts mediad, which takes a moment: say so now, not when it is up.
        if requests.iter().any(|request| matches!(request, Request::CallAccept)) {
            self.call_state.connecting = Some(bare_call(conversation.clone(), CallState::Connecting, false, false));
        }
        let state = &mut self.call_state;
        (state.error, state.local, state.ended, state.rating, state.rating_timer) = (None, None, None, None, None);
        // A call asked for here opens full-screen on a phone.
        state.phone.minimised = false;
        // The phone's platform says when the microphone is refused (`phone::hooks`).
        let phone_denied = phone::hooks::denied(Permission::Microphone);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let work = async move {
                // A picture of the dock never asks anyone for the microphone.
                let allowed = cfg!(feature = "gui-shot") || (!phone_denied && permission::ensure() != permission::Microphone::Denied);
                if !allowed {
                    return Err(None);
                }
                for request in &requests {
                    match ask(request) {
                        Ok(Response::Error { detail, .. }) => return Err(Some(detail)),
                        Ok(_) => {}
                        Err(err) => return Err(Some(crate::client::trouble_words("call", &err))),
                    }
                }
                Ok(())
            };
            let outcome = cx.background_executor().spawn(work).await;
            let _ = this.update(cx, |this, cx| {
                this.call_connecting_over(matches!(outcome, Ok(())), cx);
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
                        this.call_state.local = Some(bare_call(conversation, state, false, false));
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
        let network = conversation.network;
        self.call_state.error = None;
        cx.spawn(async move |this, cx| {
            // Ok(Some(url)): a phone opens it below, on the UI thread, through GPUI.
            let work = async move {
                match ask(&Request::Handoff { conversation }) {
                    Ok(Response::Link { url }) if super::links_through_gpui() => checked_link(&url).map(|()| Some(url)),
                    Ok(Response::Link { url }) => open(&url).map(|()| None),
                    Ok(Response::Error { detail, .. }) => Err(detail),
                    Ok(_) => Err(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => Err(crate::client::trouble_words("call", &err)),
                }
            };
            let outcome = cx.background_executor().spawn(work).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(Some(url)) => {
                        cx.open_url(&url);
                        this.toast(Some(("icons/external-link.svg", theme::text_soft())), handoff_toast(network), cx);
                    }
                    Ok(None) => {
                        this.toast(Some(("icons/external-link.svg", theme::text_soft())), handoff_toast(network), cx);
                    }
                    Err(refused) => {
                        this.call_state.error = Some(refused.clone());
                        this.refused = Some(refused);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A small header pill while a call is live elsewhere, taking you back to it in one click.
    pub(super) fn call_live_hint(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if super::phone::active() {
            return None;
        }
        let open = snapshot.open.as_ref()?;
        let call = self.call_shown(snapshot).filter(|call| in_call(call) && call.conversation != open.key)?;
        let subject = Subject::of(self, snapshot, &call);
        let label = format!("In call · {}", subject.title);
        Some(
            div()
                .id("call-live-hint")
                .flex_none()
                .h(px(28.))
                .px(px(10.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::positive_soft())
                .text_color(theme::positive())
                .hover(|style| style.bg(theme::hover()))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .tooltip(tip("Open the call", Some("⌘⇧J")))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.call_jump(cx)))
                .child(glyph("icons/phone.svg", 13., theme::positive()))
                .child(div().max_w(px(160.)).truncate().child(label)),
        )
    }

    /// The header pill, by what the open conversation can do (07 §1). Its look is the capability:
    /// a filled pill calls from here, an outlined one with the network's icon opens the official
    /// app, and a conversation that can do neither has none.
    pub(super) fn call_pill(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        ensure_keys(cx);
        ensure_daemon();
        let open = snapshot.open.as_ref()?;
        let key = open.key.clone();
        let mine = self.call_shown(snapshot).filter(|call| {
            call.conversation == key && !matches!(call.state, CallState::Failed { .. })
        });
        // On a phone the strip's actions are glyphs in 44 pt targets, Members and Details beside
        // it: the call is one more of them, its glyph alone, tinted by what it does.
        let phone = super::phone::active();
        let pill = if phone {
            div()
                .id("call")
                .group("call-pill")
                .flex_none()
                .size(px(44.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .active(|style| style.bg(theme::pressed()))
        } else {
            div()
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
        }
        // Keep the press from also dragging the window.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let words = |words: SharedString| (!phone).then_some(words);
        let glyph = |size: f32| if phone { 16. } else { size };

        // On a phone my own call is a screen of its own: the strip's glyph goes back to it.
        if let Some(call) = mine.as_ref().filter(|_| phone) {
            let live = in_call(call);
            let color = if live { theme::positive() } else { theme::text_soft() };
            return Some(
                pill.border_color(gpui::transparent_black())
                    .tooltip(tip("Back to the call", Some("⌘⇧J")))
                    .on_click(cx.listener(|this, _, _, cx| this.call_phone_restore(cx)))
                    .child(icon(if live { "icons/audio-lines.svg" } else { "icons/phone.svg" }, 18., color, None)),
            );
        }
        if let Some(call) = mine {
            let incoming = matches!(call.state, CallState::RingingIn { .. });
            let word = match call.state {
                CallState::RingingIn { .. } => "Decline",
                CallState::RingingOut | CallState::Connecting => "Cancel",
                _ => "Leave",
            };
            let hint = if incoming { ("Decline the call", "⌘⇧R") } else { ("Leave the call", "⌘⇧H") };
            return Some(
                pill.when(!phone, |pill| pill.bg(theme::danger_soft()))
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
                    .child(icon("icons/phone-off.svg", glyph(14.), theme::danger(), Some(("call-pill", theme::on_accent()))))
                    .children(words(word.into())),
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
                        pill.when(!phone, |pill| pill.bg(theme::accent_soft()))
                            .border_color(gpui::transparent_black())
                            .text_color(theme::accent())
                            .hover(|style| style.bg(theme::accent_wash()))
                            .tooltip(tip(named(there), None))
                            .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                            .child(icon("icons/phone.svg", glyph(14.), theme::accent(), None))
                            .children(words(format!("Join call · {}", there.total).into())),
                    );
                }
                let (mark, word, hint) = if voice {
                    (
                        "icons/volume-2.svg",
                        "Join voice",
                        format!("Join {}. People there will see you arrive.", open.title),
                    )
                } else {
                    ("icons/phone.svg", "Call", format!("Call {} on {}", open.title, network.label()))
                };
                Some(
                    pill.when(!phone, |pill| pill.bg(theme::raised()))
                        .border_color(theme::hairline())
                        .text_color(theme::text())
                        .hover(|style| style.bg(theme::selected()))
                        .tooltip(tip(hint, None))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                        .child(icon(mark, glyph(14.), if phone { theme::text_soft() } else { theme::text() }, None))
                        .children(words(word.into())),
                )
            }
            // Never says "Call": it is a different object, and it starts nothing here.
            // Slack shows a headphones glyph and "Huddle" so it reads as a call, not just a logo.
            CallSupport::Handoff => {
                let (glyph_icon, label) = handoff_pill_parts(network);
                let hint = handoff_tooltip(network);
                let leading = match glyph_icon {
                    Some(path) => icon(path, glyph(14.), theme::text_soft(), None).into_any_element(),
                    None => network_icon(network, glyph(14.)).into_any_element(),
                };
                Some(
                    pill.border_color(theme::border())
                        .text_color(theme::text_soft())
                        .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                        .tooltip(tip(hint, None))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_handoff(key.clone(), cx)))
                        .child(leading)
                        .children(words(label.into()))
                        .children((!phone).then(|| icon("icons/arrow-up-right.svg", 12., theme::text_faint(), None))),
                )
            }
            CallSupport::Restricted => Some(
                pill.border_color(theme::border())
                    .text_color(theme::text_faint())
                    .opacity(0.5)
                    .cursor_default()
                    .tooltip(tip(format!("{} restricts calls", open.title), None))
                    .child(icon("icons/phone-off.svg", glyph(14.), theme::text_faint(), None))
                    .children(words("Call".into())),
            ),
        }
    }
}

/// Push to talk on, and its key held or not, for a made-up call under `--shot`.
#[cfg(feature = "gui-shot")]
pub(in crate::gui) fn shot_ptt() -> Option<bool> {
    shot::ptt()
}

/// The connection's numbers of a made-up Tiny call, under `--shot`.
#[cfg(feature = "gui-shot")]
pub(in crate::gui) fn shot_stats() -> Option<protocol::CallStats> {
    shot::stats()
}

// A ring's system notification, with Accept and Decline, is chatd's (`chatd/src/notify.rs`):
// it is posted with the window closed too, and withdrawn when the ring stops.

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Occupant;

    fn key(id: &str) -> Key {
        Key { network: Network::Tiny, account: "me".into(), id: id.into() }
    }

    #[test]
    fn a_join_just_asked_for_is_drawn_connecting_until_chatd_has_a_call() {
        let asked = bare_call(key("lounge"), CallState::Connecting, false, false);
        let joined = bare_call(key("lounge"), CallState::Joined, false, false);
        let old_failure = bare_call(key("den"), CallState::Failed { reason: CallFailure::Lost, detail: String::new() }, false, false);
        assert_eq!(drawn(None, None, Some(&asked)).unwrap().state, CallState::Connecting);
        assert_eq!(drawn(Some(old_failure), None, Some(&asked)).unwrap().conversation, key("lounge"), "a failure on show gives way to the join just asked for");
        assert_eq!(drawn(Some(joined.clone()), None, Some(&asked)).unwrap().state, CallState::Joined, "a live call is never drawn over");
        assert!(drawn(None, None, None).is_none());
    }

    #[test]
    fn slack_huddle_pill_uses_headphones_and_huddle_label_and_toasts_opened_in_slack() {
        assert_eq!(handoff_pill_parts(Network::Slack), (Some("icons/headphones.svg"), "Huddle".to_owned()));
        assert_eq!(handoff_pill_parts(Network::Teams), (None, "Open in Teams".to_owned()));
        assert_eq!(handoff_toast(Network::Slack), "Opened in Slack");
        assert_eq!(handoff_toast(Network::Teams), "Opened in Teams");
    }

    #[test]
    fn a_joined_call_without_keys_is_securing_not_connected() {
        let pending = protocol::CallSecurity { mode: protocol::SecurityMode::Pending, ..Default::default() };
        let someone = protocol::Participant { id: "bea".into(), ..Default::default() };
        let mut call = bare_call(key("lounge"), CallState::Joined, false, false);
        call.security = Some(pending.clone());
        assert!(!securing(&call), "alone, there is nobody to wait on");
        call.participants = vec![someone];
        assert!(securing(&call));
        call.security = Some(protocol::CallSecurity { mode: protocol::SecurityMode::EndToEnd, ..pending });
        assert!(!securing(&call), "once the keys are there it is connected");
        call.security = None;
        assert!(!securing(&call), "a call with no encryption to set up is never securing");
    }

    #[test]
    fn connecting_is_kept_for_a_grace_until_chatd_reports_a_live_call() {
        let connecting = bare_call(key("lounge"), CallState::Connecting, false, false);
        let failed = bare_call(key("den"), CallState::Failed { reason: CallFailure::Lost, detail: String::new() }, false, false);
        assert!(!connecting_settled(true, None), "asked ok, chatd silent: keep it and arm the clear");
        assert!(!connecting_settled(true, Some(&failed)), "a failure left from the call before is not chatd's word on this one");
        assert!(connecting_settled(true, Some(&connecting)), "chatd has the call: its state is drawn");
        assert!(connecting_settled(false, None), "the ask failed: drop it at once");
        assert!(connecting_settled(false, Some(&connecting)));
    }

    #[test]
    fn a_grace_that_runs_out_drops_only_the_join_it_was_armed_for() {
        let (a, b) = (key("lounge"), key("den"));
        let mut connecting = Some(bare_call(a.clone(), CallState::Connecting, false, false));
        // A's grace runs out while A is still what is drawn: dropped.
        assert!(expire_connecting(&mut connecting, Some(&a)));
        assert!(connecting.is_none());
        // B was joined within A's grace: A's expiry leaves B's drawing alone.
        let mut connecting = Some(bare_call(b.clone(), CallState::Connecting, false, false));
        assert!(!expire_connecting(&mut connecting, Some(&a)));
        assert!(connecting.is_some());
        // And B's own expiry drops it; nothing armed, nothing dropped.
        assert!(expire_connecting(&mut connecting, Some(&b)));
        let mut connecting = Some(bare_call(b, CallState::Connecting, false, false));
        assert!(!expire_connecting(&mut connecting, None));
    }

    #[test]
    fn a_call_that_could_not_be_asked_for_wins_over_connecting() {
        let denied = bare_call(key("lounge"), CallState::Failed { reason: CallFailure::Audio, detail: "no".into() }, false, false);
        let asked = bare_call(key("lounge"), CallState::Connecting, false, false);
        assert!(matches!(drawn(None, Some(&denied), Some(&asked)).unwrap().state, CallState::Failed { .. }));
    }

    fn there(names: &[&str], total: u32) -> Occupants {
        let person = |name: &&str| Occupant {
            person: Key { network: Network::Discord, account: "1".into(), id: name.to_lowercase() },
            name: (*name).to_owned(),
            avatar: None,
            muted: false,
            deafened: false,
            server_muted: false,
            server_deafened: false,
            suppressed: false,
            streaming: false,
            camera: false,
            speaking: false,
        };
        Occupants { people: names.iter().map(person).collect(), total }
    }

    #[test]
    fn waiting_call_shortcuts_target_the_ring_and_confirm_switching_calls() {
        let call = |id: &str, state| {
            let mut call: Call = serde_json::from_value(serde_json::json!({
                "conversation": Key { network: Network::Tiny, account: "server".into(), id: id.into() },
                "state": "joined", "participants": [], "muted": false, "deafened": false,
                "joined": 1, "engine_linked": true, "video": []
            })).unwrap();
            call.state = state;
            call
        };
        let incoming = || call("peer", CallState::RingingIn { from: "Bea".into() });
        let target = |snapshot: &Snapshot| incoming_action(snapshot).map(|(ring, confirm)| (ring.conversation.id.clone(), confirm));
        let mut snapshot = Snapshot::waiting();
        snapshot.call = Some(call("room", CallState::Joined));
        assert_eq!(target(&snapshot), None);
        snapshot.ring = Some(incoming());
        assert_eq!(target(&snapshot), Some(("peer".to_owned(), true)), "waiting ring needs the same leave confirmation as its button");
        snapshot.call = None;
        assert_eq!(target(&snapshot), Some(("peer".to_owned(), false)), "no active call needs no leave confirmation");
        snapshot.ring = None;
        snapshot.call = Some(incoming());
        assert_eq!(target(&snapshot), Some(("peer".to_owned(), false)));
        snapshot.call = Some(call("peer", CallState::RingingOut));
        assert_eq!(target(&snapshot), None);
    }

    #[test]
    fn only_a_room_change_on_one_tiny_server_is_a_move() {
        let room = |network, account: &str, id: &str| Key { network, account: account.into(), id: id.into() };
        assert!(moved(&room(Network::Tiny, "a", "lounge"), &room(Network::Tiny, "a", "raid")));
        assert!(!moved(&room(Network::Tiny, "a", "lounge"), &room(Network::Tiny, "a", "lounge")));
        assert!(!moved(&room(Network::Tiny, "a", "lounge"), &room(Network::Tiny, "b", "raid")), "another server");
        assert!(moved(&room(Network::Discord, "a", "1"), &room(Network::Discord, "a", "2")), "a Discord voice channel change");
        assert!(!moved(&room(Network::Discord, "a", "1"), &room(Network::Discord, "b", "2")), "another Discord account");
    }

    #[test]
    fn the_join_pill_says_who_is_in_the_call() {
        assert_eq!(named(&there(&["Iris Navarro"], 1)), "Iris is in this call");
        assert_eq!(named(&there(&["Iris Navarro", "Jun Aoki"], 2)), "Iris and Jun are in this call");
        assert_eq!(named(&there(&["Iris Navarro", "Jun Aoki", "Dev Patel"], 3)), "Iris, Jun and 1 more are in this call");
        assert_eq!(named(&there(&[], 4)), "4 people are in this call");
    }

    #[test]
    fn handoff_labels_and_tooltips_say_what_each_network_opens() {
        assert_eq!(handoff_label(Network::Telegram), "Open chat in Telegram");
        assert_eq!(handoff_label(Network::Slack), "Open huddle in Slack");
        assert_eq!(handoff_label(Network::Teams), "Open in Teams");
        assert_eq!(
            handoff_tooltip(Network::Telegram),
            "Opens this chat in the Telegram app to start a call there. tinyclient doesn't carry Telegram calls."
        );
        assert_eq!(handoff_tooltip(Network::Slack), "Open huddle in Slack");
        assert_eq!(
            handoff_tooltip(Network::Teams),
            "Teams calls open in the Teams app. tinyclient doesn't carry them."
        );
    }

    #[test]
    fn call_go_target_opens_only_known_tiny_rooms_on_server_page() {
        let tiny_room = Key { network: Network::Tiny, account: "srv".into(), id: "room-1".into() };
        let tiny_direct = Key { network: Network::Tiny, account: "srv".into(), id: "dm-unlisted".into() };
        let discord_dm = Key { network: Network::Discord, account: "acc".into(), id: "dm-1".into() };
        assert_eq!(
            call_go_target(&tiny_room, true),
            CallGoTarget::TinyRoom(tiny_room.account(), "room-1".into())
        );
        assert_eq!(call_go_target(&tiny_direct, false), CallGoTarget::Conversation);
        assert_eq!(call_go_target(&discord_dm, false), CallGoTarget::Conversation);
    }

    #[test]
    fn gain_cache_is_keyed_by_call_account_not_shown_server() {
        let mut state = State::default();
        let tiny_a = AccountKey { network: Network::Tiny, account: "srv-a".into() };
        let tiny_b = AccountKey { network: Network::Tiny, account: "srv-b".into() };
        let discord = AccountKey { network: Network::Discord, account: "acc-1".into() };
        state.gains.insert((tiny_a.clone(), "u1".into()), (140, false));
        state.gains.insert((tiny_b.clone(), "u1".into()), (60, true));
        state.gains.insert((discord.clone(), "u1".into()), (180, false));
        assert_eq!(state.gain(&tiny_a, "u1"), (140, false));
        assert_eq!(state.gain(&tiny_b, "u1"), (60, true));
        assert_eq!(state.gain(&discord, "u1"), (180, false));
        assert_eq!(state.gain(&tiny_a, "other"), (100, false));
    }

    #[test]
    fn auto_watch_starts_once_for_direct_peer_ignores_rooms_and_respects_manual_stop() {
        let mut call: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "tiny", "account": "srv", "id": "dm-1" },
            "state": "joined",
            "participants": [{ "id": "peer-1", "name": "Bea", "sharing": true, "camera": false }],
            "muted": false, "deafened": false, "joined": 1, "engine_linked": true, "video": []
        })).unwrap();
        let mut prev = None;
        // Room call never auto-watches.
        assert_eq!(auto_watch_step(&call, true, &mut prev), None);
        assert_eq!(prev, None);
        // Direct call auto-watches once.
        assert_eq!(auto_watch_step(&call, false, &mut prev), Some(Some("peer-1".into())));
        // Subsequent Call changes (including after a manual Stop watching) do not re-ask.
        assert_eq!(auto_watch_step(&call, false, &mut prev), None);
        // When peer stops sharing, decoder stops and prev clears.
        call.participants[0].sharing = false;
        assert_eq!(auto_watch_step(&call, false, &mut prev), Some(None));
        assert_eq!(prev, None);
        // When peer turns on camera, auto-watch triggers again.
        call.participants[0].camera = true;
        assert_eq!(auto_watch_step(&call, false, &mut prev), Some(Some("peer-1".into())));
    }

    #[test]
    fn discord_speaker_picks_focused_then_debounced_speaker_then_first_camera_and_clears() {
        let mut call: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "discord", "account": "acc", "id": "vc-1" },
            "state": "joined",
            "participants": [
                { "id": "u1", "name": "Ada", "camera": true, "speaking": false },
                { "id": "u2", "name": "Bea", "camera": true, "speaking": true }
            ],
            "muted": false, "deafened": false, "joined": 1, "engine_linked": true, "video": []
        })).unwrap();
        let t0 = Instant::now();
        // Focused person with camera wins immediately.
        assert_eq!(discord_speaker_step(&call, Some("u1"), None, t0), Some(Some("u1".into())));
        // Otherwise speaking person with camera wins.
        assert_eq!(discord_speaker_step(&call, None, None, t0), Some(Some("u2".into())));
        // Switching speakers within 800 ms is debounced while current still has camera.
        call.participants[0].speaking = true;
        call.participants[1].speaking = false;
        assert_eq!(discord_speaker_step(&call, None, Some(("u2", t0)), t0 + Duration::from_millis(300)), None);
        // After 800 ms, switches to new speaker.
        assert_eq!(discord_speaker_step(&call, None, Some(("u2", t0)), t0 + Duration::from_millis(900)), Some(Some("u1".into())));
        // When nobody has camera on, clears speaker to None.
        call.participants[0].camera = false;
        call.participants[1].camera = false;
        assert_eq!(discord_speaker_step(&call, None, Some(("u1", t0)), t0 + Duration::from_secs(2)), Some(None));
    }

    #[test]
    fn call_people_count_includes_me_and_excludes_ringing() {
        let mut call = bare_call(key("lounge"), CallState::Joined, false, false);
        assert_eq!(call_people_count(&call), 1);
        call.participants = vec![
            protocol::Participant { id: "u1".into(), name: "Ada".into(), ringing: false, ..Default::default() },
            protocol::Participant { id: "u2".into(), name: "Bea".into(), ringing: true, ..Default::default() },
            protocol::Participant { id: "u3".into(), name: "Cam".into(), ringing: false, ..Default::default() },
        ];
        assert_eq!(call_people_count(&call), 3);
    }

    #[test]
    fn participant_row_keyboard_shortcuts_map_enter_and_context_menu() {
        assert_eq!(call_row_key_action("enter", false, false), Some(RowKeyAction::Profile));
        assert_eq!(call_row_key_action("enter", true, false), None);
        assert_eq!(call_row_key_action("contextmenu", false, false), Some(RowKeyAction::Menu));
        assert_eq!(call_row_key_action("f10", true, false), Some(RowKeyAction::Menu));
        assert_eq!(call_row_key_action("f10", false, false), None);
        assert_eq!(call_row_key_action("f10", true, true), None);
        assert_eq!(call_row_key_action("space", false, false), None);
    }

    #[test]
    fn share_notice_prompts_once_respects_dismiss_and_resets_when_sharing_stops() {
        let mut call = bare_call(key("lounge"), CallState::Joined, false, false);
        call.participants = vec![
            protocol::Participant { id: "u1".into(), name: "Ada".into(), sharing: false, ..Default::default() },
            protocol::Participant { id: "u2".into(), name: "Bea".into(), sharing: false, ..Default::default() },
        ];
        let (mut prev, mut prompt) = (None, None);
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), None);
        assert_eq!(prompt, None);

        // Ada starts sharing -> prompts once.
        call.participants[0].sharing = true;
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), Some("u1".into()));
        assert_eq!(prompt.as_deref(), Some("u1"));

        // Repeated Call snapshots while Ada keeps sharing do not re-trigger.
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), None);
        assert_eq!(prompt.as_deref(), Some("u1"));

        // Dismissing clears the prompt, and prev holds so it does not immediately reappear.
        prompt = None;
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), None);
        assert_eq!(prompt, None);

        // Ada stops sharing -> clears prev so the next share starts fresh.
        call.participants[0].sharing = false;
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), None);
        assert_eq!(prev, None);

        // Bea starts sharing -> prompts again.
        call.participants[1].sharing = true;
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), Some("u2".into()));
        assert_eq!(prompt.as_deref(), Some("u2"));

        // Leaving / reconnecting clears both.
        call.state = CallState::Reconnecting;
        assert_eq!(share_notice_step(&call, &mut prev, &mut prompt), None);
        assert_eq!((prev, prompt), (None, None));
    }

    #[test]
    fn share_watch_is_not_the_tiny_subscription_once_watch_was_sent() {
        assert!(!share_watch_superseded(Network::Discord, true));
        assert!(!share_watch_superseded(Network::Discord, false));
        assert!(!share_watch_superseded(Network::Tiny, false));
        assert!(share_watch_superseded(Network::Tiny, true));
    }

    #[test]
    fn auto_watch_share_requires_pref_active_view_and_no_existing_focus() {
        assert!(should_auto_watch_share(true, true, false));
        assert!(!should_auto_watch_share(false, true, false));
        assert!(!should_auto_watch_share(true, false, false));
        assert!(!should_auto_watch_share(true, true, true));
    }

    #[test]
    fn desired_watch_sources_prioritizes_focused_share_and_fills_small_camera_tiles() {
        let mut call = bare_call(key("lounge"), CallState::Joined, false, false);
        call.participants = vec![
            protocol::Participant { id: "ada".into(), name: "Ada".into(), sharing: true, camera: true, ..Default::default() },
            protocol::Participant { id: "bea".into(), name: "Bea".into(), camera: true, ..Default::default() },
            protocol::Participant { id: "cy".into(), name: "Cy".into(), camera: true, ..Default::default() },
            protocol::Participant { id: "dev".into(), name: "Dev".into(), camera: true, ..Default::default() },
            protocol::Participant { id: "eli".into(), name: "Eli".into(), camera: true, ..Default::default() },
        ];
        // Focused on Ada's share -> Large share + 4 small cameras (Ada's camera + Bea, Cy, Dev).
        let sources = desired_watch_sources(&call, Some(&stage::Focus::Stream("ada".into())), 4);
        assert_eq!(sources.len(), 5);
        assert_eq!(sources[0], protocol::WatchSource { user_id: "ada".into(), kind: protocol::PictureKind::Share, size: protocol::WatchSize::Large });
        assert_eq!(sources[1], protocol::WatchSource { user_id: "ada".into(), kind: protocol::PictureKind::Camera, size: protocol::WatchSize::Small });
        assert_eq!(sources[4].user_id, "dev");
        // Phone budget: 2 small cameras beside focused share.
        let phone = desired_watch_sources(&call, Some(&stage::Focus::Stream("ada".into())), 2);
        assert_eq!(phone.len(), 3);
        // Deafened -> nothing watched.
        call.deafened = true;
        assert!(desired_watch_sources(&call, Some(&stage::Focus::Stream("ada".into())), 4).is_empty());
    }
}
