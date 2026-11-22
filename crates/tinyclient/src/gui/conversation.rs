//! The conversation pane: its strip, the messages, and what shows when nothing is open.
//!
//! The messages are a view of their own ([`Pane`]), so scrolling them redraws nothing else. It
//! keeps a bounded run of the conversation (`pages`), lays it out as rows and dividers (`rows`),
//! and draws them (`draw`, with `rich` and `text` for the words and `media` and `images` for
//! the pictures). A thread is a second, dense `Pane` in the right pane.

mod actions;
mod draw;
mod forum;
mod emoji;
mod header;
mod icons;
pub(in crate::gui) mod images;
pub(in crate::gui) mod links;
mod media;
mod pages;
mod player;
mod rich;
mod rows;
#[cfg(feature = "gui-shot")]
mod shot;
mod popout;
mod side;
mod text;
mod text_preview;
mod tokens;
mod video;
mod viewer;
mod when;
mod wiki;
use protocol::highlight;

pub(in crate::gui) use player::glyph as player_glyph;
pub(in crate::gui) use popout::{modifier_click, modifier_held, press_opens_row};


use super::components::{button, empty_state, glyph, ButtonKind};
use super::layout::{drags_window, TOP_BAR};
use super::state::{Shell, Snapshot};
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{
    div, prelude::*, px, Bounds, ClipboardItem, Context, Div, Entity, EventEmitter,
    FocusHandle, KeyDownEvent, ListAlignment, ListOffset, ListState, MouseButton, Pixels, Point,
    Subscription, WeakEntity, Window,
};
use images::Images;
use pages::PAGE;
use protocol::{
    Change, Conversation, ConversationKind, Key, MediaFile, Message, Network, Page, Request, Response, SendState, Special,
    PAGE_LIMIT,
};
use rows::Item;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokens::Metrics;

/// The thread pane and the viewer are reached from here; `Shell` only holds this.
#[derive(Default)]
pub struct State {
    pane: OnceCell<Entity<Pane>>,
    /// A Discord forum's post list, drawn in place of the message pane.
    forum: OnceCell<Entity<forum::Board>>,
    /// The thread in the right pane, while the pane's stack holds one.
    thread: RefCell<Option<Entity<Pane>>>,
    listening: OnceCell<Subscription>,
    following: OnceCell<Subscription>,
    side: side::Side,
    /// Conversations the user opened in their own windows.
    pub(in crate::gui) windows: popout::Windows,
    /// Age-restricted channels the user clicked Continue on during this session.
    passed_age_gate: HashSet<Key>,
}

impl State {
    /// The message menu reads commands the shell just stored.
    pub(in crate::gui) fn wake_panes(&self, cx: &mut gpui::App) {
        if let Some(pane) = self.pane.get() {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(thread) = self.thread.borrow().clone() {
            thread.update(cx, |_, cx| cx.notify());
        }
    }
}

/// How often the network is told how far the user has read, at most.
const READ_EVERY: Duration = Duration::from_secs(1);
/// How far from the bottom the jump button appears.
const AWAY: f32 = 200.;
/// Rows this far outside the viewport are built, so their pictures are asked for before they
/// show and older history is asked for before the top is reached.
const AHEAD: f32 = 600.;

/// Why a page was asked for, which decides what happens to the scroll position when it lands.
#[derive(Clone, PartialEq)]
enum Purpose {
    Open,
    /// Something changed while pinned to the bottom.
    Newest,
    /// Something changed while reading further up: what is on screen is read again.
    Around,
    /// The same, for what arrived at the end meanwhile.
    Arrivals,
    Older,
    Newer,
    /// A reply quote was clicked and its target was not held.
    Jump(String),
    /// The jump button, from a window that no longer reached the end.
    Bottom,
    /// Coming back this session to where the user left it: this message, this far into it.
    Restore(String, f32),
}

/// A page that did not load. Shown until it is retried or another page lands.
struct HistoryMiss {
    words: String,
    page: Page,
    purpose: Purpose,
}

fn history_failure(purpose: &Purpose, detail: &str) -> String {
    let what = match purpose {
        Purpose::Newer => "newer messages",
        Purpose::Jump(_) => "that message",
        Purpose::Bottom => "the latest messages",
        _ => "older messages",
    };
    format!("Couldn't load {what}. {detail}")
}

fn answer_detail(answer: &io::Result<Response>) -> String {
    match answer {
        Ok(Response::Error { detail, .. }) => detail.clone(),
        Ok(_) => crate::client::OTHER_ANSWER.to_owned(),
        Err(err) => crate::client::trouble_words("load", err),
    }
}

/// Where a conversation was left, for coming back to it in this session.
#[derive(Clone, Debug, PartialEq)]
struct Place {
    /// The first message on screen, and how far its top was above the viewport's.
    id: String,
    offset: f32,
    /// Left at the newest message: it opens there again, by the landing rule.
    bottom: bool,
}

pub(super) struct Open {
    key: Key,
    title: String,
    kind: ConversationKind,
    /// My own id on this account.
    me: String,
    /// The conversation's picture: a direct one's is the other person's.
    picture: Option<MediaFile>,
    /// Where the user had read to on opening. The "New" line stays there until they leave.
    last_read: Option<String>,
    marker_known: bool,
    /// The message a thread hangs off, and how many replies it had.
    root: Option<(Rc<Message>, u32)>,
    /// What it takes as reactions, once chatd said. `None` until then, or from a chatd that
    /// does not say: everything is offered, as before there were rules.
    reactions: Option<protocol::ReactionRules>,
    /// What became of my newest message, from the conversation's row.
    receipt: Option<protocol::Receipt>,
    /// What this account may do on messages (`Capabilities`).
    can: protocol::Capabilities,
    /// Per-conversation moderation and management rights for this account.
    rights: protocol::ConversationRights,
    /// This account may delete other members' messages in this conversation.
    moderate: bool,
    /// This account may pin and unpin messages in this conversation.
    can_pin: bool,
    call: protocol::CallSupport,
    /// Conversation topic (e.g. "RCS message" / "Text message" for Google Messages).
    topic: String,
    /// Channel is marked age-restricted by the network.
    age_restricted: bool,
    /// Waiting behind the session age-gate warning until the user clicks Continue.
    gated: bool,
    /// Special conversation role (e.g. Telegram Saved Messages, exempt from the 48h edit window).
    special: protocol::Special,
    /// True when this conversation is with a bot/app (enables Home / Messages / About tabs on Slack).
    bot: bool,
    /// True when the conversation is end-to-end encrypted (`Encryption::is_sealed()`).
    encrypted: bool,
    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    started: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AppTab {
    Home,
    #[default]
    Messages,
    About,
}

#[derive(Default)]
struct ReadMark {
    /// The newest message the network has been told about, in [`Message::rank`] order.
    sent: Option<(i64, u32, u8, i64, usize, String)>,
    at: Option<Instant>,
    /// Seen since, waiting for the second to pass.
    next: Option<String>,
    waiting: bool,
    /// The user just marked this unread. Messages already on screen must not read it again
    /// until they scroll. Leaving and coming back starts a fresh mark, so the divider shows.
    suppress: bool,
    /// The first frame after the mark records where the list sat. A later move is a scroll.
    armed: bool,
    held_at: Option<(usize, f32)>,
}

/// Selected text, by message and place in it rather than by anything drawn, so it outlives
/// the rows scrolling away and back.
struct Selection {
    message: String,
    /// Where the button went down.
    anchor: rich::Place,
    /// Where the drag has reached; `None` until it moves.
    head: Option<rich::Place>,
}

impl Selection {
    /// The bytes of `block` that are selected.
    fn within(&self, block: usize) -> Option<std::ops::Range<usize>> {
        let head = self.head?;
        let (from, to) = if self.anchor <= head { (self.anchor, head) } else { (head, self.anchor) };
        if block < from.0 || block > to.0 {
            return None;
        }
        let start = if block == from.0 { from.1 } else { 0 };
        let end = if block == to.0 { to.1 } else { usize::MAX };
        (start < end).then_some(start..end)
    }
}

/// Earlier wordings opened from an edited mark.
struct Earlier {
    message: String,
    at: Point<Pixels>,
    rows: Vec<protocol::Revision>,
    /// The answer has landed. Until then the list says it is looking.
    ready: bool,
}

/// Tag members popover opened by clicking a `@Tag` mention in a Teams channel.
pub(super) struct TagPopover {
    pub space: Key,
    pub tag_id: String,
    pub label: String,
    pub at: Point<Pixels>,
    pub tag: Option<protocol::TeamTag>,
    pub ready: bool,
}

/// A Block Kit control waiting for user confirmation before firing (`ConfirmDialog`).
#[derive(Clone)]
pub(super) struct PendingControlAction {
    pub conversation: Key,
    pub message_id: String,
    pub block_id: String,
    pub action_id: String,
    pub element_type: String,
    pub value: String,
    pub values: Vec<String>,
    pub open_url: Option<String>,
    pub confirm: protocol::ConfirmDialog,
}

/// An open popover menu for a Block Kit / Discord select/overflow/date/time control.
#[derive(Clone)]
pub(super) struct OpenSelect {
    pub conversation: Key,
    pub message_id: String,
    pub block_id: String,
    pub action_id: String,
    pub element_type: String,
    pub options: Vec<protocol::ControlOption>,
    pub selected: Option<String>,
    pub multi_selected: Vec<String>,
    pub min_values: u8,
    pub max_values: u8,
    pub confirm: Option<protocol::ConfirmDialog>,
    pub at: Point<Pixels>,
}

/// An active interaction modal form popped up by a bot/app (`Change::InteractionModal`).
pub(super) struct ActiveModal {
    pub conversation: Key,
    pub modal: protocol::InteractionModal,
    pub inputs: Vec<Entity<crate::gui::input::TextInput>>,
    pub error: Option<String>,
    pub submitting: bool,
}

/// The one rule for telling the network how far the user has read: a real window that is
/// active, showing the conversation, with the message on screen. A build that takes
/// screenshots never does, whatever it is showing: a picture of a conversation must not read it.
pub(super) fn may_mark_read(screenshots: bool, window_active: bool, open: bool, on_screen: bool) -> bool {
    !screenshots && window_active && open && on_screen
}

/// The edited mark's tooltip. No time: the network only said that it was edited.
pub(super) fn edited_tip(at: Option<i64>) -> String {
    match at {
        Some(at) => format!("Edited {}", crate::gui::time::tooltip(at)),
        None => "Edited".to_owned(),
    }
}

/// One earlier wording in the list a click on the edited mark opens.
pub(super) fn revision_line(at: i64, text: &str) -> String {
    let words = text.trim();
    let words = if words.is_empty() { "(empty)" } else { words };
    format!("{} · {words}", crate::gui::time::clock(at))
}

/// Whether the "New" marker follows the newest message as pages land. The line marks the first
/// unread when the conversation opens. Once it is open, a message that arrives while the view
/// is at the newest one in the active window was seen as it came. A line that is already up
/// stays where it is until the user leaves.
fn reads_as_it_lands(opened: bool, pinned: bool, window_active: bool, divider: bool) -> bool {
    opened && pinned && window_active && !divider
}

const SCREENSHOTS: bool = cfg!(feature = "gui-shot");

/// What to do with a file once it is on disk.
#[derive(Clone, Copy, PartialEq)]
enum Then {
    QuickLook,
    /// The system's own app for it: a video plays in the system's player.
    Open,
}

pub enum PaneEvent {
    /// The reply line under this message was clicked.
    OpenThread(Rc<Message>),
}

impl EventEmitter<PaneEvent> for Pane {}

/// Where each row drawn in a frame is.
type Seen = Rc<RefCell<Vec<(usize, Bounds<Pixels>)>>>;

/// The messages of one conversation, or of one thread.
pub struct Pane {
    metrics: Metrics,
    /// A thread's pane, not a conversation's.
    thread: bool,
    shell: WeakEntity<Shell>,
    open: Option<Open>,
    window: pages::Window,
    items: Vec<Item>,
    list: ListState,
    focus: FocusHandle,

    loading_older: bool,
    loading_newer: bool,
    /// Why older history did not come.
    older_failed: Option<String>,
    /// A newer page, a jump, or the latest page that did not load.
    history_miss: Option<HistoryMiss>,
    /// Refreshes on their way. A change that arrives meanwhile is read once they land.
    refreshing: u32,
    /// Replacing reads started since this pane opened. An earlier answer does not land over a later one.
    page_epoch: u64,
    stale: bool,

    /// Scrolled more than [`AWAY`] from the bottom.
    away: bool,
    /// Messages that arrived while away.
    unseen: usize,
    read: ReadMark,
    active: bool,
    watching_window: Option<Subscription>,

    images: Images,
    wiki: wiki::Cache,
    /// The conversation changed; its pictures go at the next draw, which has the window.
    images_stale: bool,
    /// Files the rows being drawn need, and how many pixels their longest side must have.
    want: Vec<(String, u32)>,
    /// The largest size each file has been asked for at. Asked again only for more.
    asked: HashMap<String, u32>,
    /// Where each file was last seen on disk. A message read again while a larger size is on
    /// its way has no path; the smaller picture is drawn until the larger lands.
    known: HashMap<String, String>,
    /// Files whose bytes were replaced on disk; their decodes go at the next draw.
    replaced: Vec<String>,
    /// The viewer's decodes of originals that nothing shows any more, as `(path, edge)`; they
    /// go at the next draw.
    let_go: Vec<(String, u32)>,
    /// The window's scale when sizes were last asked for.
    scale: f32,
    /// Files the user asked to open, and the name each is opened under.
    then: HashMap<String, (Then, String)>,

    /// Spoilers opened, by message and span.
    revealed: HashSet<(String, u32)>,
    /// Spoilered pictures opened, by file.
    shown: HashSet<String>,
    /// Polls where the user toggled "Show results" before voting, by message id.
    poll_results: HashSet<String>,
    /// Shared-contact phone numbers revealed by clicking them, keyed by "{msg_id}:{phone}".
    revealed_contacts: HashSet<String>,
    /// Long code blocks opened, by message and block.
    expanded: HashSet<(String, usize)>,
    /// Blocked channel messages explicitly revealed by clicking "Show".
    revealed_blocked: HashSet<String>,
    /// Sensitive messages (invite links / sign-in codes) revealed while Streamer mode is active.
    revealed_sensitive: HashSet<String>,
    /// Coloured code, by message and block, so a scroll does not colour the same block again.
    code_colors: HashMap<(String, usize), (u64, Rc<highlight::Lit>)>,
    /// Blocks whose colouring is already running off this thread.
    code_coloring: HashSet<(String, usize)>,
    /// The code block just copied, which shows a check for a moment.
    copied: Option<(String, usize)>,
    /// The link under the pointer: message, block, run.
    hovered: Option<(String, usize, usize)>,
    selection: Option<Selection>,
    /// The button is down on a message's text.
    dragging: bool,
    /// The press now being handled landed on text, so it does not clear the selection.
    on_text: bool,
    /// The message a jump landed on, tinted while it fades.
    flash: Option<String>,
    viewer: Option<viewer::Viewer>,
    /// Expanded full-screen text/code attachment viewer (#385).
    text_viewer: Option<viewer::TextViewer>,
    /// Progressive/ranged media streaming progress by file id: `(path, loaded, total, complete)`.
    pub(super) stream_progress: HashMap<String, (String, u64, u64, bool)>,
    /// Decoded inline text/code attachment previews by `file.id` (`None` = binary/unpreviewable).
    text_previews: HashMap<String, Option<Rc<text_preview::TextPreview>>>,
    /// Text attachment files currently being read/decoded off the UI thread.
    text_preview_loading: HashSet<String>,
    /// Text attachment ids rendered on screen that need a ranged head preview fetch.
    want_text_previews: Vec<String>,
    /// Text attachment ids already requested via `Request::FetchPreview`.
    asked_text_previews: HashSet<String>,
    /// The message under the pointer, which shows the toolbar.
    hover: Option<String>,
    /// A phone's press on a message, where it went down, and the timer that shows the toolbar
    /// if it holds still (`actions::LONG_PRESS`). Dropping the task cancels it.
    press: Option<(gpui::Task<()>, (f32, f32))>,
    menu: Option<actions::Menu>,
    /// Where the Apps row and its submenu were painted, so a click on the submenu is not outside.
    apps_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    apps_menu: Rc<Cell<Option<Bounds<Pixels>>>>,
    picker: Option<actions::Picking>,
    /// The message the delete dialog asks about.
    confirm: Option<Rc<Message>>,
    /// Pending confirmation dialog for a paid Telegram Star reaction (#517).
    paid_react: Option<actions::PaidReactConfirm>,
    /// The message being forwarded (or copied to another conversation).
    forward: Option<actions::Forwarding>,
    /// The announcement Follow picker modal.
    follow: Option<actions::Following>,
    /// Multi-message selection mode state (#795).
    multi_select: actions::MultiSelect,
    /// The link a click asked about, until the person opens it or does not.
    link_ask: Option<links::LinkAsk>,
    /// Earlier wordings of one message, opened from its edited mark.
    revisions: Option<Earlier>,
    /// Popover listing members of a clicked Teams `@Tag` mention.
    tag_popover: Option<TagPopover>,
    /// Interactive Block Kit controls currently in flight, keyed by `(message_id, action_id)`.
    pending_actions: HashMap<(String, String), Instant>,
    /// Pending confirmation dialog for a Block Kit control.
    action_confirm: Option<PendingControlAction>,
    /// Details modal returned by an interactive control (giveaway status, game high scores, alerts).
    action_result: Option<protocol::ActionResult>,
    /// Conversation and message that modal came from, so its Allow button can send a follow-up.
    action_origin: Option<(Key, String)>,
    /// Currently open select/overflow/date/time picker popover for a Block Kit / Discord control.
    open_select: Option<OpenSelect>,
    /// Active interaction modal form opened by a bot/app.
    interaction_modal: Option<ActiveModal>,
    /// On-demand reactors fetched for messages in the open conversation, keyed by `(message_id, emoji, burst)`.
    fetched_reactors: HashMap<(String, String, bool), (Vec<protocol::Reactor>, bool)>,
    /// Reactor fetch currently in flight, keyed by `(message_id, emoji, burst)`.
    reactors_loading: Option<(String, String, bool)>,
    /// Session cache of lazily resolved invite previews (keyed by `"code"` or `"code:event_id"`).
    invite_previews: HashMap<String, Result<protocol::InvitePreview, String>>,
    /// Session cache of lazily resolved standalone scheduled events (keyed by `(guild_id, event_id)`).
    event_previews: HashMap<(String, String), Result<protocol::ScheduledEvent, String>>,
    /// Invite or event cache keys currently resolving.
    invite_loading: HashSet<String>,
    /// Invite codes currently joining.
    invite_joining: HashSet<String>,
    /// Invite codes that required captcha/verification and should hand off to the official app.
    invite_handoff: HashSet<String>,
    /// Scheduled events whose Interested state is currently toggling.
    event_toggling: HashSet<(String, String)>,
    /// Recent lazy on-screen invite/event resolve timestamps, paced to at most 4 per minute.
    invite_resolve_times: VecDeque<Instant>,
    /// The pinned bar under the header (Telegram).
    pin_bar: Option<PinBar>,
    /// Loaded Saved Messages sub-dialogs and reaction tags for the open Saved Messages chat.
    saved_loaded_for: Option<Key>,
    saved_dialogs: Vec<protocol::SavedDialog>,
    saved_tags: Vec<protocol::SavedTag>,
    /// Active Saved Messages filter `(peer_id, tag)` and its fetched messages.
    saved_filter: Option<(Option<String>, Option<String>)>,
    saved_filtered: Option<Vec<Rc<Message>>>,
    /// Queued unread mention / reaction message IDs for jumping through unread items.
    unread_jumps_for: Option<Key>,
    unread_mention_ids: Vec<String>,
    unread_reaction_ids: Vec<String>,
    /// On-demand inline translations for messages in the open conversation, keyed by `message_id`.
    /// Never written to stored message history; cleared when switching conversations.
    translations: HashMap<String, protocol::Translation>,
    /// Messages currently waiting for an inline translation response.
    translating: HashSet<String>,
    /// After an older page: the message that was on screen and how far below the top of the
    /// viewport it was, to put it back there once the rows above it can be measured.
    hold: Option<(String, f32)>,
    /// The first page put a "New" line in: it is placed a third of the way down once the rows
    /// can be measured, when what is unread does not fit.
    land: bool,
    /// Where each conversation was left this session.
    places: crate::gui::pane::Memory<Place>,
    /// The next open starts from the page around this message: more is unread than a page holds.
    around: Option<String>,
    /// A search hit asked for this message before its conversation opened; it wins over where
    /// reading stopped.
    landing: Option<String>,
    /// Deferred initial load target held while an age-restricted channel waits behind the gate.
    gated_open: Option<(Option<String>, Option<Place>, Option<String>)>,
    /// Nothing to type in: the messages take the focus on the next draw.
    wants_focus: bool,
    /// How far the files of each of my pending messages have gone, by `client_id`.
    uploads: HashMap<String, (u64, u64)>,
    /// When each pending message was first drawn, so its time and place hold until it is sent.
    pending_at: HashMap<String, i64>,

    /// A drag along a player, until it is let go.
    scrub: Option<player::Scrub>,
    /// Where each player's waveform or bar was last drawn, by file.
    scrub_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// A file the user pressed play on before it was on disk: file, message, attachment, from.
    play_after: Option<(String, String, usize, Option<u32>)>,
    /// Transcripts currently collapsed by the user, keyed by `(message_id, attachment_index)`.
    transcript_collapsed: HashSet<(String, usize)>,
    /// Transcriptions currently awaiting the initial RPC reply, keyed by `(message_id, attachment_index)`.
    transcribing: HashSet<(String, usize)>,
    /// That press was a round video message, so it plays in the circle when the file lands.
    round_after: bool,
    /// The round note muted autoplay started, stopped once it leaves the screen.
    round_auto: Option<String>,
    /// The round tile under the pointer, for hover autoplay.
    round_hover: Option<String>,
    /// The JPEG currently drawn in a round note, so the one before it can be dropped.
    round_frame: Option<(String, String)>,
    /// Bumped at each draw. A round tile sets [`Self::round_mark`] to it while it is laid out.
    round_stamp: u64,
    round_mark: u64,
    /// Draws in a row that did not lay the round tile out. A few of them and muted autoplay stops.
    round_miss: u8,
    /// Clip attachment file ids whose full transcript panel is expanded.
    transcript_open: HashSet<String>,
    /// Lazily parsed WebVTT lines keyed by VTT `MediaFile::id`.
    transcripts: HashMap<String, Vec<protocol::TranscriptLine>>,
    /// Video clip attachment file ids with inline captions toggled on.
    captions_on: HashSet<String>,

    /// How wide the list was when last drawn; 0 before the first draw.
    width: f32,
    /// A phone's first frame after opening, before the rows can be measured: a conversation
    /// that fits the viewport is then left at its start, and one that overflows opens at its
    /// newest message.
    opening: bool,
    /// Where each row drawn this frame is, for knowing what is on screen.
    seen: Seen,
    /// Coarse 60-second timer that redraws relative (`<t:unix:R>`) timestamp chips only while
    /// at least one is visible on screen. Dropped as soon as none are visible.
    relative_timer: Option<gpui::Task<()>>,
    /// Active tab for Slack app conversations (`Home`, `Messages`, `About`).
    app_tab: AppTab,
    /// Loaded About + Home tab view for the open Slack app conversation (fetched on demand only).
    app_view: Option<(Key, protocol::AppView)>,
    app_loading: bool,
    app_error: Option<String>,
    /// Ephemeral messages ("Only visible to you") for the currently open conversation; kept in
    /// memory only and cleared on conversation switch or dismissal.
    ephemeral: Vec<Rc<Message>>,
    #[cfg(feature = "gui-shot")]
    script: shot::Script,
}

impl Pane {
    /// `thread`: the pane under a message, which reads down from its root and has no
    /// conversation's own staging. A desktop draws it `DENSE`; a phone, as its conversations.
    fn new(metrics: Metrics, thread: bool, shell: WeakEntity<Shell>, cx: &mut Context<Self>) -> Pane {
        // A thread reads down from its root; a conversation up from its newest message.
        let alignment = if thread { ListAlignment::Top } else { ListAlignment::Bottom };
        Pane {
            metrics,
            thread,
            shell,
            open: None,
            window: pages::Window::default(),
            items: Vec::new(),
            list: ListState::new(0, alignment, px(AHEAD)),
            focus: cx.focus_handle(),
            loading_older: false,
            loading_newer: false,
            older_failed: None,
            history_miss: None,
            refreshing: 0,
            page_epoch: 0,
            stale: false,
            away: false,
            unseen: 0,
            read: ReadMark::default(),
            active: false,
            watching_window: None,
            images: Images::default(),
            wiki: wiki::Cache::default(),
            images_stale: false,
            want: Vec::new(),
            asked: HashMap::new(),
            known: HashMap::new(),
            replaced: Vec::new(),
            let_go: Vec::new(),
            scale: 0.,
            then: HashMap::new(),
            revealed: HashSet::new(),
            shown: HashSet::new(),
            poll_results: HashSet::new(),
            revealed_contacts: HashSet::new(),
            expanded: HashSet::new(),
            revealed_blocked: HashSet::new(),
            revealed_sensitive: HashSet::new(),
            code_colors: HashMap::new(),
            code_coloring: HashSet::new(),
            copied: None,
            hovered: None,
            selection: None,
            dragging: false,
            on_text: false,
            flash: None,
            viewer: None,
            text_viewer: None,
            text_previews: HashMap::new(),
            text_preview_loading: HashSet::new(),
            want_text_previews: Vec::new(),
            asked_text_previews: HashSet::new(),
            hover: None,
            press: None,
            menu: None,
            apps_anchor: Rc::default(),
            apps_menu: Rc::default(),
            picker: None,
            confirm: None,
            paid_react: None,
            forward: None,
            follow: None,
            multi_select: actions::MultiSelect::default(),
            link_ask: None,
            revisions: None,
            tag_popover: None,
            pending_actions: HashMap::new(),
            action_confirm: None,
            action_result: None,
            action_origin: None,
            open_select: None,
            interaction_modal: None,
            fetched_reactors: HashMap::new(),
            reactors_loading: None,
            invite_previews: HashMap::new(),
            event_previews: HashMap::new(),
            invite_loading: HashSet::new(),
            invite_joining: HashSet::new(),
            invite_handoff: HashSet::new(),
            event_toggling: HashSet::new(),
            invite_resolve_times: VecDeque::new(),
            translations: HashMap::new(),
            translating: HashSet::new(),
            hold: None,
            land: false,
            places: crate::gui::pane::Memory::default(),
            around: None,
            landing: None,
            gated_open: None,
            wants_focus: false,
            uploads: HashMap::new(),
            stream_progress: HashMap::new(),
            pending_at: HashMap::new(),
            scrub: None,
            scrub_bounds: Rc::default(),
            play_after: None,
            transcript_collapsed: HashSet::new(),
            transcribing: HashSet::new(),
            round_after: false,
            round_auto: None,
            round_hover: None,
            round_frame: None,
            round_stamp: 0,
            round_mark: 0,
            round_miss: 0,
            transcript_open: HashSet::new(),
            transcripts: HashMap::new(),
            captions_on: HashSet::new(),
            width: 0.,
            opening: false,
            seen: Rc::default(),
            relative_timer: None,
            app_tab: AppTab::Messages,
            app_view: None,
            app_loading: false,
            app_error: None,
            ephemeral: Vec::new(),
            pin_bar: None,
            saved_loaded_for: None,
            saved_dialogs: Vec::new(),
            saved_tags: Vec::new(),
            saved_filter: None,
            saved_filtered: None,
            unread_jumps_for: None,
            unread_mention_ids: Vec::new(),
            unread_reaction_ids: Vec::new(),
            #[cfg(feature = "gui-shot")]
            script: if thread { shot::Script::default() } else { shot::Script::from_env() },
        }
    }

    fn is_thread(&self) -> bool {
        self.thread
    }

    pub(super) fn toggle_poll_results(&mut self, message_id: String, cx: &mut Context<Self>) {
        if !self.poll_results.remove(&message_id) {
            self.poll_results.insert(message_id);
        }
        cx.notify();
    }

    pub(super) fn toggle_contact_reveal(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.revealed_contacts.remove(&key) {
            self.revealed_contacts.insert(key);
        }
        cx.notify();
    }

    pub(super) fn toggle_transcription(&mut self, message_id: String, index: usize, cx: &mut Context<Self>) {
        let key = (message_id, index);
        if !self.transcript_collapsed.remove(&key) {
            self.transcript_collapsed.insert(key);
        }
        cx.notify();
    }

    /// Requests server-side transcription for one voice or round video message (explicit user action only).
    pub(super) fn transcribe(&mut self, message_id: String, index: usize, cx: &mut Context<Self>) {
        if SCREENSHOTS {
            return;
        }
        let Some(conversation) = self.open.as_ref().map(|open| open.key.clone()) else {
            return;
        };
        let slot = (message_id.clone(), index);
        self.transcript_collapsed.remove(&slot);
        if !self.transcribing.insert(slot.clone()) {
            return;
        }
        cx.notify();
        cx.spawn(async move |pane, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { ask(&Request::Transcribe { conversation, message_id }) })
                .await;
            let _ = pane.update(cx, |this, cx| {
                this.transcribing.remove(&slot);
                if let Ok(Response::Error { detail, .. }) = answer {
                    let shell = this.shell.clone();
                    cx.defer(move |cx| {
                        let _ = shell.update(cx, |shell, cx| shell.toast(None, detail, cx));
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sends thumbs-up / thumbs-down feedback for a completed transcription.
    pub(super) fn rate_transcript(&mut self, message_id: String, transcription_id: i64, good: bool, cx: &mut Context<Self>) {
        if SCREENSHOTS || transcription_id == 0 {
            return;
        }
        let Some(conversation) = self.open.as_ref().map(|open| open.key.clone()) else {
            return;
        };
        cx.spawn(async move |pane, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    ask(&Request::RateTranscript {
                        conversation,
                        message_id,
                        transcription_id,
                        good,
                    })
                })
                .await;
            let _ = pane.update(cx, |this, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    let shell = this.shell.clone();
                    cx.defer(move |cx| {
                        let _ = shell.update(cx, |shell, cx| shell.toast(None, detail, cx));
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_transcript_file(&mut self, vtt_id: &str, path: &str) {
        const MAX_VTT_BYTES: u64 = 512 * 1024;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_VTT_BYTES {
                return;
            }
        }
        if let Ok(raw) = std::fs::read_to_string(path) {
            self.transcripts.insert(vtt_id.to_owned(), protocol::parse_webvtt(&raw));
        }
    }

    fn ensure_transcript_loaded(&mut self, vtt: Option<MediaFile>, cx: &mut Context<Self>) {
        let Some(vtt) = vtt else {
            return;
        };
        if self.transcripts.contains_key(&vtt.id) {
            return;
        }
        if let Some(path) = vtt.path.clone().or_else(|| self.known.get(&vtt.id).cloned()) {
            self.load_transcript_file(&vtt.id, &path);
        } else {
            self.fetch(vec![vtt.id.clone()], None, vtt.failed.is_some(), cx);
        }
    }

    pub(super) fn toggle_transcript(&mut self, clip_id: String, vtt: Option<MediaFile>, cx: &mut Context<Self>) {
        if !self.transcript_open.remove(&clip_id) {
            self.transcript_open.insert(clip_id);
            self.ensure_transcript_loaded(vtt, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_captions(&mut self, clip_id: String, vtt: Option<MediaFile>, cx: &mut Context<Self>) {
        if !self.captions_on.remove(&clip_id) {
            self.captions_on.insert(clip_id);
            self.ensure_transcript_loaded(vtt, cx);
        }
        cx.notify();
    }

    pub(super) fn transcript_lines<'a>(&'a self, transcript: &'a protocol::Transcript) -> &'a [protocol::TranscriptLine] {
        if !transcript.lines.is_empty() {
            return &transcript.lines;
        }
        if let Some(vtt) = &transcript.file {
            if let Some(lines) = self.transcripts.get(&vtt.id) {
                return lines;
            }
        }
        &[]
    }

    pub(super) fn vote_poll(&mut self, message_id: String, votes: Option<Vec<u32>>, cx: &mut Context<Self>) {
        if SCREENSHOTS {
            return;
        }
        let Some(conversation) = self.open.as_ref().map(|open| open.key.clone()) else {
            return;
        };
        cx.spawn(async move |pane, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { ask(&Request::Poll { conversation, message_id, votes }) })
                .await;
            let _ = pane.update(cx, |this, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    let shell = this.shell.clone();
                    cx.defer(move |cx| {
                        let _ = shell.update(cx, |shell, cx| shell.toast(None, detail, cx));
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows this conversation. The same one again only takes its new title, picture and the
    /// mark on my newest message.
    /// `typing`: it has a composer, which takes the focus; otherwise the messages do.
    fn show(
        &mut self,
        conversation: &Conversation,
        me: &str,
        can: protocol::Capabilities,
        picture: Option<MediaFile>,
        typing: bool,
        gated: bool,
        cx: &mut Context<Self>,
    ) {
        let receipt = conversation.receipt();
        let encrypted = conversation.encryption.is_sealed();
        if let Some(open) = self.open.as_mut().filter(|open| open.key == conversation.key) {
            let unlocked = open.gated && !gated;
            if open.title != conversation.title
                || open.picture != picture
                || open.receipt != receipt
                || open.can != can
                || open.rights != conversation.rights
                || open.moderate != conversation.moderate
                || open.can_pin != conversation.can_pin
                || open.call != conversation.call
                || open.topic != conversation.topic
                || open.age_restricted != conversation.age_restricted
                || open.gated != gated
                || open.special != conversation.special
                || open.bot != conversation.bot
                || open.encrypted != encrypted
            {
                open.title.clone_from(&conversation.title);
                open.picture = picture;
                open.receipt = receipt;
                open.can = can;
                open.rights = conversation.rights;
                open.moderate = conversation.moderate;
                open.can_pin = conversation.can_pin;
                open.call = conversation.call;
                open.topic.clone_from(&conversation.topic);
                open.age_restricted = conversation.age_restricted;
                open.gated = gated;
                open.special = conversation.special;
                open.bot = conversation.bot;
                open.encrypted = encrypted;
                if unlocked {
                    let (landing, back, around) = self.gated_open.take().unwrap_or((None, None, None));
                    self.load_opened(landing, back, around, cx);
                }
                cx.notify();
            }
            return;
        }
        // More unread than a page holds: the first page is the one around where reading stopped.
        // A search hit's landing wins over it.
        let around = conversation.last_read.clone().filter(|_| conversation.unread > PAGE / 2);
        if self.landing.is_none() {
            self.around = around;
        }
        self.start(
            conversation.key.clone(),
            &conversation.title,
            conversation.kind,
            me,
            can,
            conversation.rights,
            None,
            conversation.age_restricted,
            gated,
            cx,
        );
        self.wants_focus = !typing;
        if let Some(open) = self.open.as_mut() {
            open.picture = picture;
            open.receipt = receipt;
            open.rights = conversation.rights;
            open.moderate = conversation.moderate;
            open.can_pin = conversation.can_pin;
            open.call = conversation.call;
            open.topic.clone_from(&conversation.topic);
            open.special = conversation.special;
            open.bot = conversation.bot;
            open.encrypted = encrypted;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show_thread(
        &mut self,
        key: Key,
        title: &str,
        me: &str,
        can: protocol::Capabilities,
        rights: protocol::ConversationRights,
        root: Rc<Message>,
        age_restricted: bool,
        gated: bool,
        cx: &mut Context<Self>,
    ) {
        let replies = root.thread.as_ref().map_or(0, |thread| thread.replies);
        self.start(key, title, ConversationKind::Thread, me, can, rights, Some((root, replies)), age_restricted, gated, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        key: Key,
        title: &str,
        kind: ConversationKind,
        me: &str,
        can: protocol::Capabilities,
        rights: protocol::ConversationRights,
        root: Option<(Rc<Message>, u32)>,
        age_restricted: bool,
        gated: bool,
        cx: &mut Context<Self>,
    ) {
        let landing = self.landing.take();
        let around = self.around.take();
        self.revisions = None;
        self.relative_timer = None;
        self.translations.clear();
        self.fetched_reactors.clear();
        self.reactors_loading = None;
        self.translations.clear();
        self.translating.clear();
        // Where the one being left was, for coming back to it.
        if let Some((left, place)) = self.key().cloned().zip(self.place()) {
            self.places.put(left, place);
        }
        let back = self.places.get(&key).filter(|place| !place.bottom).cloned();
        self.open = Some(Open {
            key,
            title: title.to_owned(),
            kind,
            me: me.to_owned(),
            picture: None,
            last_read: None,
            marker_known: false,
            root,
            reactions: None,
            receipt: None,
            can,
            rights,
            moderate: false,
            can_pin: false,
            call: protocol::CallSupport::None,
            topic: String::new(),
            age_restricted,
            gated,
            special: protocol::Special::None,
            bot: false,
            encrypted: false,
            started: Instant::now(),
        });
        self.app_tab = AppTab::Messages;
        self.app_view = None;
        self.app_loading = false;
        self.app_error = None;
        self.ephemeral.clear();
        self.window = pages::Window::default();
        self.items = Vec::new();
        self.list.reset(0);
        self.loading_older = false;
        self.loading_newer = false;
        self.older_failed = None;
        self.history_miss = None;
        self.refreshing = 0;
        self.page_epoch = 0;
        self.stale = false;
        self.away = false;
        self.unseen = 0;
        self.read = ReadMark::default();
        self.images_stale = true;
        self.want.clear();
        self.asked.clear();
        self.known.clear();
        self.replaced.clear();
        self.let_go.clear();
        self.then.clear();
        self.revealed.clear();
        self.shown.clear();
        self.expanded.clear();
        self.revealed_blocked.clear();
        self.transcript_open.clear();
        self.transcripts.clear();
        self.captions_on.clear();
        self.revealed_sensitive.clear();
        self.code_colors.clear();
        self.multi_select.clear();
        self.code_coloring.clear();
        self.copied = None;
        self.hovered = None;
        self.selection = None;
        self.dragging = false;
        self.flash = None;
        self.viewer = None;
        self.text_viewer = None;
        self.text_previews.clear();
        self.text_preview_loading.clear();
        self.want_text_previews.clear();
        self.asked_text_previews.clear();
        self.hover = None;
        self.menu = None;
        self.picker = None;
        self.confirm = None;
        self.paid_react = None;
        self.action_confirm = None;
        self.action_result = None;
        self.action_origin = None;
        self.open_select = None;
        self.interaction_modal = None;
        self.pending_actions.clear();
        self.forward = None;
        self.follow = None;
        self.link_ask = None;
        self.tag_popover = None;
        self.hold = None;
        self.land = false;
        self.opening = false;
        self.gated_open = None;
        self.uploads.clear();
        self.pending_at.clear();
        self.relay(true, ListOffset::default());
        if gated {
            self.gated_open = Some((landing, back, around));
        } else {
            self.load_opened(landing, back, around, cx);
        }
        cx.notify();
    }

    fn is_gated(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.gated)
    }

    fn load_opened(
        &mut self,
        landing: Option<String>,
        back: Option<Place>,
        around: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.ask_reactions(cx);
        // M2: a conversation opens at its newest message rather than the first unread. The
        // search hit is the user's own ask: it wins over where this conversation was left, and
        // lands as a jump, flashed, with the pill.
        let around = around.filter(|_| !super::app::prefs::choices().newest);
        match landing {
            Some(id) => self.load(Page::Around { id: id.clone() }, Purpose::Jump(id), cx),
            None => match back {
                Some(place) => self.load(Page::Around { id: place.id.clone() }, Purpose::Restore(place.id, place.offset), cx),
                None => match around {
                    Some(id) => self.load(Page::Around { id }, Purpose::Open, cx),
                    None => self.load(Page::Newest, Purpose::Open, cx),
                },
            },
        }
    }

    fn pass_age_gate(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if !open.gated || open.can.age_restricted_blocked {
            return;
        }
        open.gated = false;
        let key = open.key.clone();
        let (landing, back, around) = self.gated_open.take().unwrap_or((None, None, None));
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            let _ = shell.update(cx, |shell, cx| shell.pass_age_gate(key, cx));
        });
        self.load_opened(landing, back, around, cx);
        cx.notify();
    }

    fn age_gate_view(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let blocked = self.open.as_ref().is_some_and(|open| open.can.age_restricted_blocked);
        let title = self.open.as_ref().map(|open| open.title.clone()).unwrap_or_default();
        let shell = self.shell.clone();
        let continue_btn = button("age-gate-continue", ButtonKind::Primary, "Continue")
            .when(blocked, |btn| btn.opacity(0.45).cursor_default())
            .when(!blocked, |btn| {
                btn.on_click(cx.listener(|this, _, _, cx| this.pass_age_gate(cx)))
            });
        let nope_btn = button("age-gate-nope", ButtonKind::Secondary, "Nope").on_click(cx.listener(move |_, _, _, cx| {
            let _ = shell.update(cx, |shell, cx| shell.leave_age_gated(cx));
        }));
        div()
            .id("age-gate")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(24.))
            .child(
                div()
                    .max_w(px(380.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(48.))
                            .rounded_full()
                            .bg(theme::danger_soft())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph("icons/triangle-alert.svg", 22., theme::danger())),
                    )
                    .child(
                        div()
                            .type_title()
                            .text_color(theme::text())
                            .text_align(gpui::TextAlign::Center)
                            .child(if title.is_empty() {
                                "Age-restricted channel".to_owned()
                            } else {
                                format!("#{} is age-restricted", title.trim_start_matches('#'))
                            }),
                    )
                    .child(
                        div()
                            .type_ui()
                            .text_color(theme::text_soft())
                            .text_align(gpui::TextAlign::Center)
                            .child("This channel may contain content not suitable for all ages."),
                    )
                    .children(blocked.then(|| {
                        div()
                            .type_small()
                            .text_color(theme::danger())
                            .text_align(gpui::TextAlign::Center)
                            .child("Your account isn't permitted to view age-restricted channels.")
                    }))
                    .child(
                        div()
                            .pt(px(4.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(continue_btn)
                            .child(nope_btn),
                    ),
            )
    }

    /// Where the user is in this conversation now: the first message on screen and how far
    /// into it, as a place that stays true however the rows above it are measured later.
    fn place(&self) -> Option<Place> {
        if self.is_thread() || self.items.is_empty() {
            return None;
        }
        let bottom = !self.away;
        let top = self.list.logical_scroll_top();
        let (id, offset) = match self.items.get(top.item_ix).and_then(Item::message) {
            Some(message) => (message.id.clone(), f32::from(top.offset_in_item)),
            None => (self.items.get(top.item_ix..)?.iter().find_map(Item::message)?.id.clone(), 0.),
        };
        Some(Place { id, offset, bottom })
    }

    fn key(&self) -> Option<&Key> {
        self.open.as_ref().map(|open| &open.key)
    }

    /// The message with this id, wherever it is held.
    fn find(&self, id: &str) -> Option<Rc<Message>> {
        let root = self.open.as_ref().and_then(|open| open.root.as_ref()).map(|(root, _)| root);
        root.filter(|root| root.id == id).or_else(|| self.window.messages.iter().find(|m| m.id == id)).cloned()
    }

    // Pages.

    fn load(&mut self, page: Page, purpose: Purpose, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let limit = match purpose {
            Purpose::Newest => (self.window.messages.len() as u32).clamp(PAGE, PAGE_LIMIT),
            Purpose::Around => PAGE * 2,
            _ => PAGE,
        };
        // The paging measurement has a second to cross all of history.
        #[cfg(feature = "gui-shot")]
        let limit = if purpose == Purpose::Older && self.script.paging() { PAGE_LIMIT } else { limit };
        match purpose {
            Purpose::Older | Purpose::Open | Purpose::Restore(..) => self.loading_older = true,
            Purpose::Newer => self.loading_newer = true,
            Purpose::Newest | Purpose::Around | Purpose::Arrivals => self.refreshing += 1,
            Purpose::Jump(_) | Purpose::Bottom => {}
        }
        // Open does not count as a refresh, so a history change can start a second read
        // while the first is still in flight. The later replacing read wins. Older, newer,
        // and arrival pages stay beside it.
        let asked = if replaces_window(&purpose) {
            self.page_epoch = self.page_epoch.saturating_add(1);
            self.page_epoch
        } else {
            self.page_epoch
        };
        let request = Request::Messages { conversation: conversation.clone(), page: page.clone(), limit };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| this.loaded(&conversation, &page, purpose, asked, answer, cx));
        })
        .detach();
    }

    fn loaded(
        &mut self,
        conversation: &Key,
        page: &Page,
        purpose: Purpose,
        asked: u64,
        answer: io::Result<Response>,
        cx: &mut Context<Self>,
    ) {
        if self.key() != Some(conversation) {
            return;
        }
        match purpose {
            Purpose::Older | Purpose::Open | Purpose::Restore(..) => self.loading_older = false,
            Purpose::Newer => self.loading_newer = false,
            Purpose::Newest | Purpose::Around | Purpose::Arrivals => self.refreshing = self.refreshing.saturating_sub(1),
            Purpose::Jump(_) | Purpose::Bottom => {}
        }
        // A page asked earlier must not land last and put an empty or blank window over the
        // rows a later read already showed. The current page still lands, so a store that is
        // really empty still clears the pane.
        if stale_page(&purpose, asked, self.page_epoch) {
            if self.stale && self.refreshing == 0 {
                self.stale = false;
                self.refresh(cx);
            }
            cx.notify();
            return;
        }
        let (messages, last_read, older, newer) = match answer {
            Ok(Response::Messages { messages, last_read, older, newer, .. }) => (messages, last_read, older, newer),
            other => {
                if let Purpose::Restore(..) = purpose {
                    // The place is gone from the history: open as if for the first time.
                    self.load(Page::Newest, Purpose::Open, cx);
                    return;
                }
                if matches!(purpose, Purpose::Jump(_)) && self.window.messages.is_empty() {
                    // The message a search hit asked for is gone from the history, and nothing
                    // is held beside it: open as if for the first time.
                    self.load(Page::Newest, Purpose::Open, cx);
                    return;
                }
                let detail = answer_detail(&other);
                if matches!(purpose, Purpose::Older | Purpose::Open) {
                    self.older_failed = Some(detail);
                    if purpose == Purpose::Open {
                        // Nothing is known about this conversation, least of all that it is
                        // empty: the row that says so, with its Retry, stands in for the start.
                        self.window.older = true;
                        self.relay(true, ListOffset::default());
                    }
                    cx.notify();
                } else if matches!(purpose, Purpose::Newer | Purpose::Jump(_) | Purpose::Bottom) {
                    self.history_miss = Some(HistoryMiss {
                        words: history_failure(&purpose, &detail),
                        page: page.clone(),
                        purpose: purpose.clone(),
                    });
                    cx.notify();
                }
                return;
            }
        };
        let opened = self.open.as_ref().is_some_and(|open| open.marker_known) && !self.window.messages.is_empty();
        if let Some(open) = self.open.as_mut().filter(|open| !open.marker_known) {
            open.last_read = last_read;
            open.marker_known = true;
            // What was read before opening is not told to the network again.
            let read = open.last_read.as_deref().and_then(|id| messages.iter().find(|message| message.id == id));
            self.read.sent = read.map(|message| message.rank());
        }
        self.older_failed = None;
        self.history_miss = None;

        let pinned = self.pinned();
        let top = self.list.logical_scroll_top();
        let reading = self.items.get(top.item_ix..).and_then(|below| below.iter().find_map(Item::message)).cloned();
        let last = self.window.messages.last().cloned();
        if purpose == Purpose::Older && !pinned {
            self.hold = self.held_place();
        }
        self.window.merge(page, messages, older, newer);
        let keep = match purpose {
            Purpose::Older => 0,
            Purpose::Around | Purpose::Jump(_) => {
                let at = reading.as_ref().and_then(|message| self.window.position(&message.id));
                at.unwrap_or(self.window.messages.len() / 2)
            }
            Purpose::Restore(ref id, _) => self.window.position(id).unwrap_or(self.window.messages.len() / 2),
            _ => self.window.messages.len().saturating_sub(1),
        };
        self.window.trim(keep);

        // What arrives under the reader's eyes is read: the marker moves on with it, so no "New"
        // line is drawn above a message that came in while they were looking.
        let divider = self.items.iter().any(|item| matches!(item, Item::New));
        if reads_as_it_lands(opened, pinned, self.active, divider) && !self.window.newer {
            if let (Some(open), Some(newest)) = (self.open.as_mut(), self.window.messages.last()) {
                open.last_read = Some(newest.id.clone());
            }
        }

        // What joined the end since: mine sends the view to it, anyone else's is counted.
        let arrived = last.and_then(|last| self.window.position(&last.id)).map(|at| &self.window.messages[at + 1..]);
        let mine = arrived.is_some_and(|new| new.iter().any(|message| message.outgoing && message.state == SendState::Pending));
        if !pinned && !mine {
            self.unseen += arrived.map_or(0, |new| new.iter().filter(|message| !message.outgoing).count());
        }
        self.relay(pinned || mine, top);
        match &purpose {
            Purpose::Jump(id) => self.reveal(id, cx),
            Purpose::Bottom => self.snap_to_end(),
            Purpose::Restore(id, offset) => self.restore(id, *offset),
            // Unread that may not fit: placed once the rows can be measured.
            Purpose::Open => {
                self.land = !super::app::prefs::choices().newest && self.items.iter().any(|item| matches!(item, Item::New));
                // A phone decides where the open lands once the rows are measured; placing
                // unread wins over both.
                self.opening = self.metrics == Metrics::PHONE && !self.thread && !self.land;
            }
            _ if mine => self.snap_to_end(),
            _ => {}
        }
        #[cfg(feature = "gui-shot")]
        if purpose == Purpose::Open {
            shot::opened(self, cx);
        }
        if self.stale && self.refreshing == 0 {
            self.stale = false;
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Something in this conversation changed: what is on screen is read again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.open.is_none() || self.is_gated() {
            return;
        }
        if self.refreshing > 0 {
            self.stale = true;
            return;
        }
        let top = self.list.logical_scroll_top().item_ix;
        let reading = self.items.get(top..).and_then(|below| below.iter().find_map(Item::message));
        match reading.filter(|_| !self.pinned()) {
            Some(reading) if self.window.position(&reading.id).is_some() => {
                let around = Page::Around { id: reading.id.clone() };
                let end = self.window.messages.last().filter(|_| !self.window.newer).map(|last| last.id.clone());
                self.load(around, Purpose::Around, cx);
                if let Some(id) = end {
                    self.load(Page::After { id }, Purpose::Arrivals, cx);
                }
            }
            _ => self.load(Page::Newest, Purpose::Newest, cx),
        }
    }

    /// The view follows the newest message.
    fn pinned(&self) -> bool {
        if self.is_thread() {
            return self.list.is_scrolled_to_end() == Some(true);
        }
        self.list.logical_scroll_top().item_ix >= self.items.len()
    }

    /// Lays the held messages out again and tells the list which rows changed, keeping what the
    /// user is reading where it is.
    fn relay(&mut self, pinned: bool, top: ListOffset) {
        let Some(open) = &self.open else {
            return;
        };
        let messages = held_in_place(&self.window.messages, &mut self.pending_at);
        let new = rows::build(&messages, &rows::Build {
            last_read: open.last_read.as_deref(),
            older: self.window.older,
            root: open.root.clone(),
            same_day: super::time::same_day,
        });
        if let Some((range, count)) = rows::changed(&self.items, &new) {
            self.list.splice(range, count);
            if pinned {
                if self.is_thread() {
                    self.list.scroll_to_end();
                }
            } else if let Some((item_ix, offset)) = rows::anchor(&self.items, &new, top.item_ix, top.offset_in_item.into()) {
                self.list.scroll_to(ListOffset { item_ix, offset_in_item: px(offset) });
            }
        }
        self.items = new;
    }

    /// Scrolls by a distance. Following the newest message, the list counts from the end of
    /// its rows rather than the top of the viewport, so the first step up starts a viewport higher.
    fn scroll_by(&mut self, distance: Pixels) {
        if self.read.suppress {
            self.read.suppress = false;
            self.read.armed = false;
            self.read.held_at = None;
        }
        let following = !self.is_thread() && self.pinned();
        let viewport = self.list.viewport_bounds().size.height;
        self.list.scroll_by(if following && distance < px(0.) { distance - viewport } else { distance });
    }

    /// Back where the user left it: the same message at the same depth.
    fn restore(&mut self, id: &str, offset: f32) {
        let Some(at) = self.items.iter().position(|item| item.message().is_some_and(|message| message.id == id)) else {
            return;
        };
        self.list.scroll_to(ListOffset { item_ix: at, offset_in_item: px(offset) });
        self.away = true;
    }

    /// Lands on the first unread (07 R3): when what is unread does not fit, the "New" line sits
    /// a third of the way down and the jump button counts what is below; otherwise it stays at
    /// the bottom with the line in view. Rows are measured, not guessed, so the line lands
    /// where it is said to.
    fn settle_land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.land {
            return;
        }
        let viewport = self.list.viewport_bounds().size;
        let (height, width) = (f32::from(viewport.height), f32::from(viewport.width));
        if height <= 0. || width <= 0. {
            // Not laid out yet: the next draw does it.
            return;
        }
        self.land = false;
        let Some(new) = self.items.iter().position(|item| matches!(item, Item::New)) else {
            return;
        };
        let space = gpui::size(gpui::AvailableSpace::Definite(px(width)), gpui::AvailableSpace::MinContent);
        let tall = |this: &mut Self, ix: usize, window: &mut Window, cx: &mut Context<Self>| {
            f32::from(this.row(ix, window, cx).layout_as_root(space, window, cx).height)
        };
        let mut below = 0.;
        for ix in new..self.items.len() {
            below += tall(self, ix, window, cx);
            if below > height {
                break;
            }
        }
        let Some(at) = landing(below, height, new, |ix| tall(self, ix, window, cx)) else {
            return;
        };
        self.list.scroll_to(at);
        self.away = true;
        self.unseen = self.items[new..].iter().filter_map(Item::message).filter(|message| !message.outgoing).count();
        cx.notify();
    }

    fn snap_to_end(&mut self) {
        self.list.scroll_to(ListOffset { item_ix: self.items.len(), offset_in_item: px(0.) });
        self.unseen = 0;
        self.away = false;
    }

    /// The jump button: the "New" line when it is still below what is on screen, else the
    /// newest message.
    fn jump(&mut self, cx: &mut Context<Self>) {
        let new = self.items.iter().position(|item| matches!(item, Item::New));
        match new.filter(|ix| self.list.item_is_below_viewport(*ix) == Some(true)) {
            Some(ix) => {
                // One row of context above the line.
                self.list.scroll_to(ListOffset { item_ix: ix.saturating_sub(1), offset_in_item: px(0.) });
                cx.notify();
            }
            None => self.go_to_bottom(cx),
        }
    }

    /// The newest message, read again if the window had left it behind.
    fn go_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.window.newer {
            self.load(Page::Newest, Purpose::Bottom, cx);
        } else {
            self.snap_to_end();
        }
        cx.notify();
    }

    /// Scrolls to a message, reading the history around it first if it is not held.
    pub(in crate::gui) fn jump_to(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.window.position(id).is_some() {
            self.reveal(id, cx);
        } else {
            self.load(Page::Around { id: id.to_owned() }, Purpose::Jump(id.to_owned()), cx);
        }
    }

    /// The next open starts on this message: a search hit opening its conversation.
    pub(in crate::gui) fn land_next(&mut self, id: String) {
        self.landing = Some(id);
    }

    fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(at) = self.items.iter().position(|item| item.message().is_some_and(|message| message.id == id)) else {
            return;
        };
        // One row of context above, so the target is not flush against the strip.
        self.list.scroll_to(ListOffset { item_ix: at.saturating_sub(1), offset_in_item: px(0.) });
        self.flash = Some(id.to_owned());
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(draw::FLASH + Duration::from_millis(100)).await;
            let _ = this.update(cx, |this, cx| {
                if this.flash.as_deref() == Some(&id) {
                    this.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    // Changes.

    fn changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        if self.is_gated() {
            return;
        }
        let Some(key) = self.key() else {
            return;
        };
        match change {
            Change::Messages { conversation } if conversation == key => self.refresh(cx),
            Change::Ephemeral { conversation, message } if conversation == key => {
                if self.ephemeral.len() >= 10 {
                    self.ephemeral.remove(0);
                }
                self.ephemeral.push(Rc::new((**message).clone()));
                cx.notify();
            }
            Change::Upload { conversation, client_id, sent, total } if conversation == key => {
                self.uploads.insert(client_id.clone(), (*sent, *total));
                cx.notify();
            }
            Change::Media { account, file } if *account == key.account() => self.landed(file, true, cx),
            Change::MediaProgress { account, id, path, loaded, total, complete } if *account == key.account() => {
                self.stream_progress.insert(id.clone(), (path.clone(), *loaded, *total, *complete));
                cx.notify();
            }
            Change::InteractionModal { conversation, modal } if conversation == key => {
                let inputs = modal
                    .fields
                    .iter()
                    .map(|field| {
                        let placeholder = field.placeholder.clone();
                        let initial = field.value.clone();
                        let multiline = field.multiline;
                        cx.new(|cx| {
                            let mut input = if multiline {
                                crate::gui::input::TextInput::multiline(placeholder, cx)
                            } else {
                                crate::gui::input::TextInput::new(placeholder, cx)
                            };
                            if !initial.is_empty() {
                                input.set_text(&initial, cx);
                            }
                            input
                        })
                    })
                    .collect();
                self.pending_actions.clear();
                self.interaction_modal = Some(ActiveModal {
                    conversation: conversation.clone(),
                    modal: modal.clone(),
                    inputs,
                    error: None,
                    submitting: false,
                });
                cx.notify();
            }
            Change::InteractionFailed { conversation, message_id, action_id, reason } if conversation == key => {
                if !message_id.is_empty() && !action_id.is_empty() {
                    self.pending_actions.remove(&(message_id.clone(), action_id.clone()));
                } else {
                    self.pending_actions.clear();
                }
                if !reason.is_empty() {
                    self.say(reason.clone(), cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// chatd says where a file is, or why it is not. `fresh` when it has only now been written:
    /// a larger size lands at the same path, so what was decoded from the old bytes is stale.
    fn landed(&mut self, file: &MediaFile, fresh: bool, cx: &mut Context<Self>) {
        if let Some(path) = &file.path {
            if fresh && self.known.get(&file.id) == Some(path) {
                self.replaced.push(path.clone());
            }
            self.known.insert(file.id.clone(), path.clone());
            if file.id.ends_with(":vtt") {
                self.load_transcript_file(&file.id, path);
            }
            if let Some((then, name)) = self.then.remove(&file.id) {
                draw::open_file(path, &name, then, cx);
            }
            if self.text_viewer.as_ref().is_some_and(|v| v.file_id == file.id) {
                self.text_viewer_landed(path, cx);
            }
            self.play_landed(&file.id, cx);
        } else if file.failed.is_some() {
            self.then.remove(&file.id);
            if let Some(orig_id) = file.id.strip_prefix(text_preview::HEAD_PREFIX) {
                self.text_previews.insert(orig_id.to_owned(), None);
            }
            if self.text_viewer.as_ref().is_some_and(|v| v.file_id == file.id) {
                if let Some(viewer) = self.text_viewer.as_mut() {
                    viewer.loading_full = false;
                }
            }
            if self.play_after.as_ref().is_some_and(|(wanted, ..)| *wanted == file.id) {
                self.play_after = None;
            }
        }
        let mut changed = self.window.landed(file);
        if let Some((root, _)) = self.open.as_mut().and_then(|open| open.root.as_mut()) {
            if root.files().any(|held| held.id == file.id && pages::differs(held, file)) {
                for held in Rc::make_mut(root).files_mut().filter(|held| held.id == file.id) {
                    pages::take(held, file);
                }
                changed = true;
            }
        }
        if changed {
            let pinned = self.pinned();
            self.relay(pinned, self.list.logical_scroll_top());
        }
        cx.notify();
    }

    /// Notes that a row on screen shows `file` with its longest side `size` device pixels. A
    /// file that failed is never asked for here: only the user's Retry does that.
    fn want(&mut self, file: &MediaFile, size: u32) {
        if self.is_gated() {
            return;
        }
        if file.failed.is_none() {
            self.want.push((file.id.clone(), size));
        }
    }

    /// Asks chatd for the files the rows on screen need, at the sizes they are drawn. Each is
    /// asked for once, and again only when a box grows or the window moves to a sharper screen.
    fn fetch_wanted(&mut self, cx: &mut Context<Self>) {
        if self.is_gated() {
            self.want.clear();
            self.want_text_previews.clear();
            return;
        }
        let wanted = pages::unasked(std::mem::take(&mut self.want), &mut self.asked);
        for (size, ids) in wanted {
            self.fetch(ids, Some(size), false, cx);
        }
        self.fetch_text_previews(cx);
    }

    /// Requests ranged head previews (capped at 100 KB) for text/code attachments currently drawn
    /// on screen.
    fn fetch_text_previews(&mut self, cx: &mut Context<Self>) {
        if self.is_gated() {
            self.want_text_previews.clear();
            return;
        }
        let mut ids = Vec::new();
        for id in std::mem::take(&mut self.want_text_previews) {
            if self.asked_text_previews.insert(id.clone()) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return;
        }
        let Some(account) = self.key().map(Key::account) else {
            return;
        };
        let request = Request::FetchPreview {
            account: account.clone(),
            ids,
            max_bytes: protocol::TEXT_PREVIEW_MAX_BYTES,
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let Ok(Response::Media { files }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.key().map(Key::account) != Some(account) {
                    return;
                }
                for file in files.iter().filter(|file| file.path.is_some() || file.failed.is_some()) {
                    this.landed(file, false, cx);
                }
            });
        })
        .detach();
    }

    fn fetch(&mut self, ids: Vec<String>, size: Option<u32>, retry: bool, cx: &mut Context<Self>) {
        if self.is_gated() {
            return;
        }
        let Some(account) = self.key().map(Key::account) else {
            return;
        };
        let request = Request::Fetch { retry, size, span: None, account: account.clone(), ids };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let Ok(Response::Media { files }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.key().map(Key::account) != Some(account) {
                    return;
                }
                // What is already on disk, or already known to have failed, is answered here;
                // the rest arrives as changes.
                for file in files.iter().filter(|file| file.path.is_some() || file.failed.is_some()) {
                    this.landed(file, false, cx);
                }
            });
        })
        .detach();
    }

    /// The user clicked Retry on a file that failed: one request, which clears the failure.
    fn retry_file(&mut self, file: &MediaFile, cx: &mut Context<Self>) {
        let size = self.asked.get(&file.id).copied();
        let mut cleared = file.clone();
        cleared.failed = None;
        // Shown as loading at once; chatd's word on it replaces this.
        self.window.landed(&cleared);
        self.images.forget_failed();
        let pinned = self.pinned();
        self.relay(pinned, self.list.logical_scroll_top());
        self.fetch(vec![file.id.clone()], size, true, cx);
        cx.notify();
    }

    /// One byte window of a moving picture. The answer is not the file, so it is not landed.
    fn fetch_span(&mut self, id: String, span: (u64, u64), cx: &mut Context<Self>) {
        let Some(account) = self.key().map(Key::account) else {
            return;
        };
        let request = Request::Fetch { retry: false, size: None, span: Some(span), account, ids: vec![id] };
        cx.spawn(async move |_this, cx| {
            let _ = cx.background_executor().spawn(async move { ask(&request) }).await;
        })
        .detach();
    }

    /// Fetches a whole file because the user asked for it, and does `then` when it is here.
    fn fetch_then(&mut self, attachment: &protocol::Attachment, then: Then, cx: &mut Context<Self>) {
        let file = &attachment.file;
        let name = media::disk_name(attachment);
        if let Some(path) = file.path.clone().or_else(|| self.known.get(&file.id).cloned()) {
            draw::open_file(&path, &name, then, cx);
            return;
        }
        self.then.insert(file.id.clone(), (then, name));
        // The user asked, so a file that failed before is tried again.
        self.fetch(vec![file.id.clone()], None, file.failed.is_some(), cx);
        cx.notify();
    }

    /// The first message on screen and how far its top is below the top of the viewport.
    fn held_place(&self) -> Option<(String, f32)> {
        let viewport = self.list.viewport_bounds();
        let seen = self.seen.borrow();
        let mut rows: Vec<&(usize, Bounds<Pixels>)> = seen.iter().filter(|(_, bounds)| bounds.bottom() > viewport.top()).collect();
        rows.sort_by_key(|(ix, _)| *ix);
        rows.into_iter().find_map(|(ix, bounds)| {
            let message = self.items.get(*ix).and_then(Item::message)?;
            Some((message.id.clone(), f32::from(bounds.top() - viewport.top())))
        })
    }

    /// Puts the held message back where it was. The rows that arrived above it have never been
    /// drawn, so they are measured here, upward, until they cover the distance.
    fn settle_hold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, below)) = self.hold.take() else {
            return;
        };
        #[cfg(feature = "gui-shot")]
        self.script.held(&id, below);
        let Some(mut at) = self.items.iter().position(|item| item.message().is_some_and(|message| message.id == id)) else {
            return;
        };
        if self.width <= 0. {
            return;
        }
        let mut left = below;
        if left <= 0. {
            self.list.scroll_to(ListOffset { item_ix: at, offset_in_item: px(-left) });
            return;
        }
        while left > 0. && at > 0 {
            at -= 1;
            let mut row = self.row(at, window, cx);
            let space = gpui::size(gpui::AvailableSpace::Definite(px(self.width)), gpui::AvailableSpace::MinContent);
            let tall = f32::from(row.layout_as_root(space, window, cx).height);
            if tall >= left {
                self.list.scroll_to(ListOffset { item_ix: at, offset_in_item: px(tall - left) });
                return;
            }
            left -= tall;
        }
        self.list.scroll_to(ListOffset::default());
    }

    // After each draw.

    /// Runs once the rows are laid out: asks for what they need, and notes how far the user
    /// has read. Nothing here draws.
    /// A phone decides where an open lands once its rows can be measured. A conversation that
    /// fits is left at its start, where the list's own bottom clamp rests its last message on
    /// the composer with the whole start block in view; one that overflows opens at its newest
    /// message. GPUI's bottom-aligned list clamps a fitting list to the bottom by itself, so the
    /// fit case is offset 0 and the overflow case is the end.
    fn settle_open(&mut self, seen: &[(usize, Bounds<Pixels>)], viewport: Bounds<Pixels>) {
        if !self.opening || self.items.is_empty() || viewport.size.height <= px(0.) {
            return;
        }
        let last = self.items.len() - 1;
        // Both ends drawn: the span between them is the whole content, wherever the list has
        // put it. A missing start row means the list is taller than the viewport.
        let start = seen.iter().find(|(ix, _)| *ix == 0).map(|(_, bounds)| bounds.top());
        let end = seen.iter().find(|(ix, _)| *ix == last).map(|(_, bounds)| bounds.bottom());
        let (Some(start), Some(end)) = (start, end) else {
            if seen.iter().any(|(ix, _)| *ix == last) {
                self.opening = false;
                self.list.scroll_to_end();
                self.unseen = 0;
                self.away = false;
            }
            return;
        };
        self.opening = false;
        if end - start > viewport.size.height {
            self.list.scroll_to_end();
            self.unseen = 0;
            self.away = false;
        } else {
            self.list.scroll_to(ListOffset::default());
        }
    }

    /// Muted autoplay stops once the round note has not been laid out for a few draws.
    /// Sound keeps going: the now-playing bar is the circle that stays.
    fn settle_round(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.round_auto.clone() else {
            return;
        };
        if self.round_mark == self.round_stamp {
            self.round_miss = 0;
            return;
        }
        self.round_miss = self.round_miss.saturating_add(1);
        if self.round_miss < 8 {
            return;
        }
        let account = self.key().map(protocol::Key::account);
        let muted = account.as_ref().is_some_and(|account| {
            self.shell.upgrade().is_some_and(|shell| {
                shell.read(cx).playing_state.of(account, &file).is_some_and(|playback| {
                    playback.muted && matches!(playback.state, protocol::PlayState::Playing | protocol::PlayState::Loading)
                })
            })
        });
        if muted {
            let _ = self.shell.update(cx, |shell, cx| shell.play_stop(cx));
        }
        self.round_auto = None;
        self.round_miss = 0;
    }

    fn after_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle_round(cx);
        self.images.sweep(window, cx);
        let scale = window.scale_factor();
        if scale != self.scale {
            // Another screen: every size asked for was for the old one.
            if self.scale != 0. {
                self.asked.clear();
                cx.notify();
            }
            self.scale = scale;
        }
        self.fetch_wanted(cx);
        self.point_at(window.mouse_position(), cx);
        let viewport = self.list.viewport_bounds();
        let width = f32::from(viewport.size.width);
        if self.land {
            cx.notify();
        }
        if width != self.width {
            // Pictures are sized to the pane, so a new width lays the rows out again.
            self.width = width;
            cx.notify();
        }
        let seen = self.seen.borrow().clone();
        self.settle_open(&seen, viewport);
        let on_screen = |bounds: &Bounds<Pixels>| bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom();

        let built = |ix: usize| seen.iter().any(|(seen, _)| *seen == ix);
        if self.window.older && !self.loading_older && self.older_failed.is_none() && built(0) {
            match self.window.messages.first() {
                Some(first) => self.load(Page::Before { id: first.id.clone() }, Purpose::Older, cx),
                // The first page never came: Retry asks for it again.
                None => self.load(Page::Newest, Purpose::Open, cx),
            }
        }
        let last = self.items.len().saturating_sub(1);
        let newer_held = self
            .history_miss
            .as_ref()
            .is_some_and(|miss| miss.purpose == Purpose::Newer);
        if self.window.newer && !self.loading_newer && !newer_held && built(last) {
            if let Some(newest) = self.window.messages.last() {
                self.load(Page::After { id: newest.id.clone() }, Purpose::Newer, cx);
            }
        }

        let end = seen.iter().find(|(ix, _)| *ix == last).map(|(_, bounds)| bounds.bottom());
        let away = !self.items.is_empty() && end.is_none_or(|end| end - viewport.bottom() > px(AWAY));
        if away != self.away {
            self.away = away;
            if !away {
                self.unseen = 0;
            }
            cx.notify();
        }

        #[cfg(feature = "gui-shot")]
        shot::drawn(self, window, cx);
        self.active = window.is_window_active();
        let has_relative_time = seen
            .iter()
            .filter(|(_, bounds)| on_screen(bounds))
            .filter_map(|(ix, _)| self.items.get(*ix).and_then(Item::message))
            .any(|message| message.spans.iter().any(|span| matches!(span.style, protocol::Style::Time { form: 'R', .. })));
        self.sync_relative_timer(has_relative_time, cx);
        self.resolve_visible_invites(&seen, viewport, cx);
        let newest = seen
            .iter()
            .filter(|(_, bounds)| on_screen(bounds))
            .filter_map(|(ix, _)| self.items.get(*ix).and_then(Item::message))
            .filter(|message| message.state == SendState::Sent)
            .max_by_key(|message| message.rank())
            .cloned();
        self.note_unread_hold();
        if !self.read.suppress {
            if let Some(newest) = newest.filter(|_| may_mark_read(SCREENSHOTS, self.active, self.open.is_some(), true)) {
                self.saw(&newest, cx);
            }
        }
    }

    fn sync_relative_timer(&mut self, needed: bool, cx: &mut Context<Self>) {
        if !needed || SCREENSHOTS {
            self.relative_timer = None;
            return;
        }
        if self.relative_timer.is_some() {
            return;
        }
        self.relative_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(60)).await;
            let _ = this.update(cx, |this, cx| {
                this.relative_timer = None;
                cx.notify();
            });
        }));
    }

    /// Resolves server invites and scheduled-event links on messages currently visible on screen,
    /// paced to at most 4 lookups per minute and cached for the session.
    fn resolve_visible_invites(&mut self, seen: &[(usize, Bounds<Pixels>)], viewport: Bounds<Pixels>, cx: &mut Context<Self>) {
        if SCREENSHOTS {
            return;
        }
        let Some(account) = self.open.as_ref().map(|open| open.key.account()) else {
            return;
        };
        let on_screen = |bounds: &Bounds<Pixels>| bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom();
        let mut to_resolve: Vec<protocol::MessageInvite> = Vec::new();
        for (ix, bounds) in seen {
            if !on_screen(bounds) {
                continue;
            }
            if let Some(message) = self.items.get(*ix).and_then(Item::message) {
                for invite in &message.invites {
                    if invite.state == protocol::InviteState::Unresolved {
                        to_resolve.push(invite.clone());
                    }
                }
            }
        }
        if to_resolve.is_empty() {
            return;
        }
        let now = Instant::now();
        while self
            .invite_resolve_times
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            self.invite_resolve_times.pop_front();
        }
        for invite in to_resolve {
            if self.invite_resolve_times.len() >= 4 {
                break;
            }
            if !invite.code.is_empty() {
                let event_id = invite.event.as_ref().map(|e| e.id.clone()).filter(|id| !id.is_empty());
                let cache_key = match &event_id {
                    Some(ev) => format!("{}:{ev}", invite.code),
                    None => invite.code.clone(),
                };
                if self.invite_previews.contains_key(&cache_key) || !self.invite_loading.insert(cache_key.clone()) {
                    continue;
                }
                self.invite_resolve_times.push_back(now);
                let req = Request::InvitePreview {
                    account: account.clone(),
                    code: invite.code.clone(),
                    event_id,
                };
                let key_for_cb = cache_key.clone();
                cx.spawn(async move |this, cx| {
                    let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
                    let _ = this.update(cx, |this, cx| {
                        this.invite_loading.remove(&key_for_cb);
                        let outcome = match answer {
                            Ok(Response::InvitePreview { preview }) => Ok(preview),
                            Ok(Response::Error { detail, .. }) => Err(detail),
                            _ => Err("Invite invalid or expired.".to_owned()),
                        };
                        if this.invite_previews.len() >= 200 {
                            this.invite_previews.clear();
                        }
                        this.invite_previews.insert(key_for_cb, outcome);
                        cx.notify();
                    });
                })
                .detach();
            } else if let Some(ev) = invite.event.as_ref().filter(|e| !e.guild_id.is_empty() && !e.id.is_empty()) {
                let ev_key = (ev.guild_id.clone(), ev.id.clone());
                let load_key = format!("ev:{}:{}", ev.guild_id, ev.id);
                if self.event_previews.contains_key(&ev_key) || !self.invite_loading.insert(load_key.clone()) {
                    continue;
                }
                self.invite_resolve_times.push_back(now);
                let req = Request::EventPreview {
                    space: Key::new(&account, ev.guild_id.clone()),
                    event_id: ev.id.clone(),
                };
                cx.spawn(async move |this, cx| {
                    let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
                    let _ = this.update(cx, |this, cx| {
                        this.invite_loading.remove(&load_key);
                        let outcome = match answer {
                            Ok(Response::EventPreview { event }) => Ok(event),
                            Ok(Response::Error { detail, .. }) => Err(detail),
                            _ => Err("Event unavailable.".to_owned()),
                        };
                        if this.event_previews.len() >= 200 {
                            this.event_previews.clear();
                        }
                        this.event_previews.insert(ev_key, outcome);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn open_invite_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.open.as_ref().map(|open| open.key.account()) else {
            return;
        };
        let key = Key::new(&account, space_id);
        let shell = self.shell.clone();
        cx.defer(move |cx| {
            let _ = shell.update(cx, |shell, cx| {
                shell.go(crate::gui::sidebar::Mode::Space(key), cx);
            });
        });
    }

    pub(super) fn join_invite(&mut self, code: String, cx: &mut Context<Self>) {
        if SCREENSHOTS || !self.invite_joining.insert(code.clone()) {
            return;
        }
        let Some(account) = self.open.as_ref().map(|open| open.key.account()) else {
            self.invite_joining.remove(&code);
            return;
        };
        cx.notify();
        let req = Request::InviteJoin { account, code: code.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                this.invite_joining.remove(&code);
                match answer {
                    Ok(Response::InviteJoined { space, .. }) => {
                        for (k, v) in &mut this.invite_previews {
                            if k == &code || k.starts_with(&format!("{code}:")) {
                                if let Ok(preview) = v {
                                    preview.joined = true;
                                }
                            }
                        }
                        if let Some(space) = space {
                            let shell = this.shell.clone();
                            cx.defer(move |cx| {
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.go(crate::gui::sidebar::Mode::Space(space), cx);
                                    shell.toast(Some(("icons/check.svg", theme::text_soft())), "Joined server", cx);
                                });
                            });
                        }
                    }
                    Ok(Response::Error { detail, .. }) => {
                        let lower = detail.to_ascii_lowercase();
                        if lower.contains("captcha") || lower.contains("verify") {
                            this.invite_handoff.insert(code.clone());
                        }
                        let shell = this.shell.clone();
                        cx.defer(move |cx| {
                            let _ = shell.update(cx, |shell, cx| shell.toast(None, detail, cx));
                        });
                    }
                    _ => {
                        let shell = this.shell.clone();
                        cx.defer(move |cx| {
                            let _ = shell.update(cx, |shell, cx| shell.toast(None, "Could not join server.".to_owned(), cx));
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn toggle_event_interest(&mut self, guild_id: String, event_id: String, interested: bool, cx: &mut Context<Self>) {
        if SCREENSHOTS {
            return;
        }
        let Some(account) = self.open.as_ref().map(|open| open.key.account()) else {
            return;
        };
        let key = (guild_id.clone(), event_id.clone());
        if !self.event_toggling.insert(key.clone()) {
            return;
        }
        cx.notify();
        let req = Request::EventLinkInterest {
            space: Key::new(&account, guild_id),
            event_id: event_id.clone(),
            interested,
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                this.event_toggling.remove(&key);
                match answer {
                    Ok(Response::Ok) => {
                        if let Some(Ok(ev)) = this.event_previews.get_mut(&key) {
                            if ev.interested != interested {
                                ev.interested = interested;
                                if let Some(c) = ev.interested_count.as_mut() {
                                    *c = if interested { c.saturating_add(1) } else { c.saturating_sub(1) };
                                }
                            }
                        }
                        for cached in this.invite_previews.values_mut() {
                            if let Ok(preview) = cached {
                                if let Some(ev) = preview.event.as_mut().filter(|e| e.id == event_id) {
                                    if ev.interested != interested {
                                        ev.interested = interested;
                                        if let Some(c) = ev.interested_count.as_mut() {
                                            *c = if interested { c.saturating_add(1) } else { c.saturating_sub(1) };
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Response::Error { detail, .. }) => {
                        let shell = this.shell.clone();
                        cx.defer(move |cx| {
                            let _ = shell.update(cx, |shell, cx| shell.toast(None, detail, cx));
                        });
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A mark-unread holds until the list moves. The frame that sets it only records the place.
    fn note_unread_hold(&mut self) {
        if !self.read.suppress {
            return;
        }
        let top = self.list.logical_scroll_top();
        let offset: f32 = top.offset_in_item.into();
        if !self.read.armed {
            self.read.armed = true;
            self.read.held_at = Some((top.item_ix, offset));
            return;
        }
        let moved = self.read.held_at.is_some_and(|(row, was)| top.item_ix != row || (offset - was).abs() > 24.);
        if moved {
            self.read.suppress = false;
            self.read.armed = false;
            self.read.held_at = None;
        }
    }

    /// The open conversation was marked unread from outside this pane. The divider sits above
    /// the newest message, and the messages already on screen are not read again.
    pub(super) fn hold_unread(&mut self, cx: &mut Context<Self>) {
        let previous = self.window.messages.len().checked_sub(2).map(|ix| self.window.messages[ix].id.clone());
        if let Some(open) = self.open.as_mut() {
            open.last_read = previous;
        }
        self.read.suppress = true;
        self.read.armed = false;
        self.read.held_at = None;
        self.read.next = None;
        let top = self.list.logical_scroll_top();
        self.relay(self.pinned(), top);
        cx.notify();
    }

    /// `message` is the first one that counts as unread. The network hears that, and this pane
    /// does not immediately read it again.
    fn mark_message_unread(&mut self, message: &Message, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let previous = self
            .window
            .position(&message.id)
            .and_then(|ix| ix.checked_sub(1))
            .map(|ix| self.window.messages[ix].id.clone());
        if let Some(open) = self.open.as_mut() {
            open.last_read = previous;
        }
        self.read.suppress = true;
        self.read.armed = false;
        self.read.held_at = None;
        self.read.next = None;
        let top = self.list.logical_scroll_top();
        self.relay(false, top);
        let message_id = message.id.clone();
        cx.background_executor().spawn(async move {
            ask(&Request::MarkUnread { conversation, message_id })
        }).detach();
        cx.notify();
    }

    /// The user has this message on screen in the active window. The network hears of the
    /// newest such message at most once a second.
    fn saw(&mut self, message: &Message, cx: &mut Context<Self>) {
        if self.read.suppress {
            return;
        }
        let order = message.rank();
        if self.read.sent.as_ref().is_some_and(|sent| *sent >= order) {
            return;
        }
        let since = self.read.at.map(|at| at.elapsed());
        if since.is_none_or(|since| since >= READ_EVERY) {
            self.mark_read(order, cx);
            return;
        }
        self.read.next = Some(message.id.clone());
        if self.read.waiting {
            return;
        }
        self.read.waiting = true;
        let wait = READ_EVERY - since.unwrap_or_default();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.read.waiting = false;
                let next = this.read.next.take().and_then(|id| this.find(&id));
                // Only if it is still true: the window may have lost focus meanwhile.
                if let Some(next) = next.filter(|_| may_mark_read(SCREENSHOTS, this.active, this.open.is_some(), true)) {
                    this.mark_read(next.rank(), cx);
                }
            });
        })
        .detach();
    }

    fn mark_read(&mut self, order: (i64, u32, u8, i64, usize, String), cx: &mut Context<Self>) {
        // The last gate before the socket, whoever called.
        if !may_mark_read(SCREENSHOTS, self.active, self.open.is_some(), true) {
            return;
        }
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let request = Request::MarkRead { conversation, message_id: order.5.clone() };
        self.read.sent = Some(order);
        self.read.at = Some(Instant::now());
        cx.background_executor().spawn(async move { ask(&request) }).detach();
    }

    // Keys and the clipboard.

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_viewer.is_some() {
            if self.text_viewer_key(event, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.viewer.is_some() {
            if self.viewer_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.action_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.message_shortcut(event, window, cx) {
            return;
        }
        // Never further than the rows already built: the list knows no heights beyond them.
        let page = (self.list.viewport_bounds().size.height - px(40.)).min(px(AHEAD - 40.));
        let keys = &event.keystroke;
        match keys.key.as_str() {
            "pageup" => self.scroll_by(-page),
            "pagedown" => {
                self.scroll_by(page);
                if self.list.is_scrolled_to_end() == Some(true) && !self.window.newer {
                    self.snap_to_end();
                }
            }
            "home" => self.list.scroll_to(ListOffset::default()),
            "end" => self.go_to_bottom(cx),
            "c" if crate::gui::shortcuts::primary(keys.modifiers) && self.selected_text().is_some() => self.copy_selection(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// E, R, +, T, P, U and Delete while a message row has focus. A menu, a confirm and a picker
    /// keep their own keys. Pin and mark-unread-from-here have no request, so they are taken and
    /// not sent. Up on an empty composer is decided in the keymap and not read here.
    fn message_shortcut(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.menu.is_some() || self.picker.is_some() || self.confirm.is_some() {
            return false;
        }
        let Some(act) = crate::gui::keymap::with_live(|map| map.message_action(&event.keystroke, crate::gui::keymap::Focus::Message, event.keystroke.is_ime_in_progress())) else {
            return false;
        };
        cx.stop_propagation();
        let Some(message) = self.shortcut_message() else {
            return true;
        };
        let allowed = self.can(&message);
        match act {
            crate::gui::keymap::MessageAct::Edit => {
                if allowed.edit {
                    self.edit(&message, window, cx);
                }
            }
            crate::gui::keymap::MessageAct::Reply => {
                if allowed.reply {
                    self.reply(&message, window, cx);
                }
            }
            crate::gui::keymap::MessageAct::React => {
                if allowed.act && self.adds(&message) {
                    self.open_picker(&message.id, window.mouse_position(), window, cx);
                }
            }
            crate::gui::keymap::MessageAct::Thread => {
                if allowed.thread {
                    cx.emit(PaneEvent::OpenThread(message));
                }
            }
            crate::gui::keymap::MessageAct::Delete => {
                if allowed.delete {
                    self.delete(&message, event.keystroke.modifiers.shift, window, cx);
                }
            }
            crate::gui::keymap::MessageAct::Pin | crate::gui::keymap::MessageAct::Unread | crate::gui::keymap::MessageAct::EditLast => {}
        }
        true
    }

    /// The message under the pointer, or the last one that is not a system line.
    fn shortcut_message(&self) -> Option<Rc<Message>> {
        let hovered = self.hover.as_deref().and_then(|id| self.find(id)).filter(|message| !matches!(message.kind, protocol::MessageKind::System));
        hovered.or_else(|| self.window.messages.iter().rev().find(|message| !matches!(message.kind, protocol::MessageKind::System)).cloned())
    }

    /// What is selected, as plain text.
    fn selected_text(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let head = selection.head?;
        let message = self.find(&selection.message)?;
        let blocks = rich::blocks(&message.text, &message.spans);
        let text = rich::plain(&blocks, selection.anchor, head);
        (!text.is_empty()).then_some(text)
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// A whole message's text as it would be typed: what "Copy text" copies.
    pub fn message_text(&self, message_id: &str) -> Option<String> {
        let message = self.find(message_id)?;
        let blocks = rich::blocks(&message.text, &message.spans);
        Some(rich::plain(&blocks, (0, 0), (blocks.len(), usize::MAX)))
    }

    /// Triggers an interactive message control, showing its confirmation dialog first if required.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn trigger_control(
        &mut self,
        conversation: Key,
        message_id: String,
        block_id: String,
        action_id: String,
        element_type: String,
        value: String,
        values: Vec<String>,
        confirm: Option<protocol::ConfirmDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_select = None;
        if self.key() != Some(&conversation) {
            return;
        }
        if let Some(confirm) = confirm {
            self.action_confirm = Some(PendingControlAction {
                conversation,
                message_id,
                block_id,
                action_id,
                element_type,
                value,
                values,
                open_url: None,
                confirm,
            });
            cx.notify();
            return;
        }
        self.execute_control(conversation, message_id, block_id, action_id, element_type, value, values, window, cx);
    }

    /// Triggers a Block Kit link button, showing its confirmation dialog first if required.
    pub(super) fn trigger_link_control(
        &mut self,
        conversation: Key,
        message_id: String,
        url: String,
        confirm: Option<protocol::ConfirmDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_select = None;
        if self.key() != Some(&conversation) {
            return;
        }
        if let Some(confirm) = confirm {
            self.action_confirm = Some(PendingControlAction {
                conversation,
                message_id,
                block_id: String::new(),
                action_id: String::new(),
                element_type: "button".to_owned(),
                value: String::new(),
                values: Vec::new(),
                open_url: Some(url),
                confirm,
            });
            cx.notify();
            return;
        }
        // A mini app is handed to Telegram's own app. Every other tg:// link stays closed:
        // a login token must not be opened from a message.
        if links::telegram_app_link(&url) {
            cx.open_url(&url);
            return;
        }
        self.open_external_link(&url, window, cx);
    }

    /// Sends `Request::MessageAction` off-thread, tracking pending state and handling alerts/links/toasts.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute_control(
        &mut self,
        conversation: Key,
        message_id: String,
        block_id: String,
        action_id: String,
        element_type: String,
        value: String,
        values: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.key() != Some(&conversation) {
            return;
        }
        let key = (message_id.clone(), action_id.clone());
        self.pending_actions.insert(key.clone(), Instant::now());
        cx.notify();
        let origin = (conversation.clone(), message_id.clone());
        let request = Request::MessageAction {
            conversation,
            message_id,
            block_id,
            action_id,
            element_type,
            value,
            values,
        };
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending_actions.remove(&key);
                match answer {
                    Ok(Response::Ok) => {}
                    Ok(Response::ActionResult { result }) => {
                        if !result.alert && result.title.is_empty() && result.fields.is_empty() && result.follow_action.is_empty() {
                            if let Some(url) = &result.url {
                                this.open_external_link(url, window, cx);
                            } else if !result.text.is_empty() {
                                this.say(result.text, cx);
                            }
                        } else {
                            this.action_origin = Some(origin);
                            this.action_result = Some(result);
                        }
                    }
                    Ok(Response::Error { detail, .. }) => this.say(detail, cx),
                    Err(err) => this.say(crate::client::trouble_words("run action", &err), cx),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn press_inline_button(
        &mut self,
        conversation: Key,
        message_id: String,
        pending_key: String,
        data: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (message_id.clone(), pending_key);
        self.pending_actions.insert(key.clone(), Instant::now());
        cx.notify();
        let request = Request::PressButton { conversation, message_id, data };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                this.pending_actions.remove(&key);
                match answer {
                    Ok(Response::ButtonAnswer { answer }) => {
                        if let Some(text) = answer.message.filter(|s| !s.is_empty()) {
                            this.say(text, cx);
                        }
                        if let Some(url) = answer.url.filter(|s| !s.is_empty()) {
                            cx.open_url(&url);
                        }
                    }
                    Ok(Response::Error { detail, .. }) => this.say(detail, cx),
                    Err(err) => this.say(crate::client::trouble_words("press button", &err), cx),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn jump_next_unread(&mut self, kind: protocol::UnreadItemKind, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        if self.unread_jumps_for.as_ref() != Some(&conversation) {
            self.unread_jumps_for = Some(conversation.clone());
            self.unread_mention_ids.clear();
            self.unread_reaction_ids.clear();
        }
        let queue = match kind {
            protocol::UnreadItemKind::Mentions => &mut self.unread_mention_ids,
            protocol::UnreadItemKind::Reactions => &mut self.unread_reaction_ids,
        };
        if let Some(id) = queue.first().cloned() {
            queue.remove(0);
            let done = queue.is_empty();
            self.jump_to(&id, cx);
            let conv = conversation.clone();
            let message_ids = protocol::jump_mark_ids(kind, &id, done);
            cx.spawn(async move |_, cx| {
                let _ = cx
                    .background_executor()
                    .spawn(async move {
                        crate::client::ask(&Request::MarkUnreadItemsRead {
                            conversation: conv,
                            kind,
                            message_ids,
                        })
                    })
                    .await;
            })
            .detach();
            cx.notify();
            return;
        }
        let conv = conversation.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::UnreadItems {
                        conversation: conv.clone(),
                        kind,
                        limit: 20,
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.key() != Some(&conversation) {
                    return;
                }
                if let Ok(Response::UnreadItems { mut message_ids }) = answer {
                    if let Some(first) = message_ids.first().cloned() {
                        message_ids.remove(0);
                        let done = message_ids.is_empty();
                        match kind {
                            protocol::UnreadItemKind::Mentions => this.unread_mention_ids = message_ids,
                            protocol::UnreadItemKind::Reactions => this.unread_reaction_ids = message_ids,
                        }
                        this.jump_to(&first, cx);
                        let conv_mark = conversation.clone();
                        let mark_ids = protocol::jump_mark_ids(kind, &first, done);
                        cx.spawn(async move |_, cx| {
                            let _ = cx
                                .background_executor()
                                .spawn(async move {
                                    crate::client::ask(&Request::MarkUnreadItemsRead {
                                        conversation: conv_mark,
                                        kind,
                                        message_ids: mark_ids,
                                    })
                                })
                                .await;
                        })
                        .detach();
                    } else {
                        let conv_mark = conversation.clone();
                        cx.spawn(async move |_, cx| {
                            let _ = cx
                                .background_executor()
                                .spawn(async move {
                                    crate::client::ask(&Request::MarkUnreadItemsRead {
                                        conversation: conv_mark,
                                        kind,
                                        message_ids: Vec::new(),
                                    })
                                })
                                .await;
                        })
                        .detach();
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn unread_jump_buttons(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.thread {
            return None;
        }
        let key = self.key()?.clone();
        let (mentions, reactions) = self.shell.upgrade().and_then(|shell| {
            let shell = shell.read(cx);
            let snap = shell.snapshot.as_ref()?;
            let can = snap.accounts.iter().find(|a| a.key == key.account())?.can.unread_jumps;
            if !can {
                return None;
            }
            let conv = snap.open.as_ref()?;
            (conv.key == key).then_some((conv.mentions, conv.unread_reactions))
        })?;
        if mentions == 0 && reactions == 0 {
            return None;
        }
        // Sit above the "jump to latest" pill while that pill is on screen.
        let bottom = if self.away {
            if self.metrics == Metrics::PHONE { px(68.) } else { px(48.) }
        } else {
            px(16.)
        };
        let mut stack = div()
            .absolute()
            .bottom(bottom)
            .right(px(16.))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.));
        if reactions > 0 {
            stack = stack.child(
                div()
                    .id("jump-unread-reactions")
                    .h(px(32.))
                    .px(px(10.))
                    .rounded_full()
                    .bg(theme::overlay())
                    .border_1()
                    .border_color(theme::border())
                    .shadow(theme::shadow_popover())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .type_small()
                    .font_weight(theme::SEMIBOLD)
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .child(format!("♥ {reactions}"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_next_unread(protocol::UnreadItemKind::Reactions, cx);
                    })),
            );
        }
        if mentions > 0 {
            stack = stack.child(
                div()
                    .id("jump-unread-mentions")
                    .h(px(32.))
                    .px(px(10.))
                    .rounded_full()
                    .bg(theme::accent())
                    .border_1()
                    .border_color(theme::accent())
                    .shadow(theme::shadow_popover())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .type_small()
                    .font_weight(theme::SEMIBOLD)
                    .text_color(theme::on_accent())
                    .cursor_pointer()
                    .child(format!("@ {mentions}"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_next_unread(protocol::UnreadItemKind::Mentions, cx);
                    })),
            );
        }
        Some(stack)
    }

    fn load_saved_dialogs(&mut self, key: Key, cx: &mut Context<Self>) {
        if self.saved_loaded_for.as_ref() == Some(&key) {
            return;
        }
        self.saved_loaded_for = Some(key.clone());
        self.saved_dialogs.clear();
        self.saved_tags.clear();
        self.saved_filter = None;
        self.saved_filtered = None;
        let account = key.account();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { crate::client::ask(&Request::SavedDialogs { account }) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.saved_loaded_for.as_ref() != Some(&key) {
                    return;
                }
                if let Ok(Response::SavedDialogs { dialogs, tags }) = answer {
                    this.saved_dialogs = dialogs;
                    this.saved_tags = tags;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply_saved_filter(
        &mut self,
        peer_id: Option<String>,
        tag: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if peer_id.is_none() && tag.is_none() {
            self.saved_filter = None;
            self.saved_filtered = None;
            cx.notify();
            return;
        }
        let Some(key) = self.key().cloned() else {
            return;
        };
        let account = key.account();
        self.saved_filter = Some((peer_id.clone(), tag.clone()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let req = Request::SavedHistory { account, peer_id, tag, limit: 50 };
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.key() != Some(&key) {
                    return;
                }
                if let Ok(Response::SavedHistory { messages }) = answer {
                    this.saved_filtered = Some(messages.into_iter().map(Rc::new).collect());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// One line under the header: the pin on show, which of how many, and the way to the list.
    /// A click jumps to the pin and puts the next older one on show.
    fn pin_bar(&mut self, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        let open = self.open.as_ref().filter(|_| !self.thread)?;
        if open.key.network != Network::Telegram || !open.can.pins || self.window.messages.is_empty() {
            return None;
        }
        let key = open.key.clone();
        // The pins are asked again when the page held says a different set is pinned.
        let seen = pins_seen(self.window.messages.iter().filter(|message| message.pinned).map(|message| message.id.as_str()));
        if !self.pin_bar.as_ref().is_some_and(|bar| bar.key == key && bar.seen == seen) {
            self.load_pin_bar(key.clone(), seen, cx);
        }
        let bar = self.pin_bar.as_ref().filter(|bar| !bar.pins.is_empty())?;
        let at = bar.at.min(bar.pins.len() - 1);
        let message = bar.pins[at].clone();
        let said = if !message.text.is_empty() {
            message.text.lines().next().unwrap_or_default().to_owned()
        } else {
            message.attachments.first().map_or_else(|| "Attachment".to_owned(), |file| file.name.clone())
        };
        Some(
            div()
                .id("pin-bar")
                .flex_none()
                .px(px(theme::PANE_PAD))
                .py(px(6.))
                .border_b_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .child(icons::icon(icons::Icon::Pin, 14., theme::accent()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().type_micro().text_color(theme::accent()).child(pin_bar_title(at, bar.pins.len())))
                        .child(div().truncate().type_small().text_color(theme::text_soft()).child(said)),
                )
                .child(
                    div()
                        .id("pin-bar-list")
                        .flex_none()
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(theme::R_CONTROL))
                        .type_micro()
                        .text_color(theme::text_soft())
                        .hover(|style| style.bg(theme::pressed()).text_color(theme::text()))
                        .child("All pins")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            let _ = this.shell.update(cx, |shell, cx| shell.open_pins(cx));
                        })),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.jump_to(&message.id, cx);
                    if let Some(bar) = this.pin_bar.as_mut() {
                        bar.at = next_pin(at, bar.pins.len());
                    }
                    cx.notify();
                })),
        )
    }

    fn load_pin_bar(&mut self, key: Key, seen: u64, cx: &mut Context<Self>) {
        // The pins on show stay until the answer lands, so the bar does not blink.
        let kept = self.pin_bar.take().filter(|bar| bar.key == key);
        let (pins, at) = kept.map_or((Vec::new(), 0), |bar| (bar.pins, bar.at));
        self.pin_bar = Some(PinBar { key: key.clone(), seen, pins, at });
        cx.spawn(async move |this, cx| {
            let request = Request::Pinned { conversation: key.clone() };
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(bar) = this.pin_bar.as_mut().filter(|bar| bar.key == key) else {
                    return;
                };
                if let Ok(Response::Pinned { messages, .. }) = answer {
                    bar.pins = messages.into_iter().map(Rc::new).collect();
                    bar.at = bar.at.min(bar.pins.len().saturating_sub(1));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The pins changed from here (the menu or the pins pane): the bar asks again.
    pub(in crate::gui) fn refresh_pin_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(bar) = self.pin_bar.as_ref() {
            self.load_pin_bar(bar.key.clone(), bar.seen, cx);
        }
    }

    fn saved_filter_bar(&mut self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.thread {
            return None;
        }
        let key = self.key()?.clone();
        let is_saved = self.shell.upgrade().is_some_and(|shell| {
            let shell = shell.read(cx);
            let conv_saved = shell
                .snapshot
                .as_ref()
                .and_then(|s| s.open.as_ref())
                .is_some_and(|c| c.key == key && c.special == protocol::Special::Saved);
            let can = shell
                .snapshot
                .as_ref()
                .and_then(|s| s.accounts.iter().find(|a| a.key == key.account()))
                .is_some_and(|a| a.can.saved_subdialogs);
            conv_saved && can
        });
        if !is_saved {
            return None;
        }
        self.load_saved_dialogs(key, cx);
        if self.saved_dialogs.is_empty() && self.saved_tags.is_empty() {
            return None;
        }
        let mut chips = div().flex().flex_wrap().items_center().gap(px(4.));
        let all_active = self.saved_filter.is_none();
        chips = chips.child(
            div()
                .id("saved-filter-all")
                .px(px(8.))
                .py(px(3.))
                .rounded(px(theme::R_CONTROL))
                .bg(if all_active { theme::accent() } else { theme::hover() })
                .text_color(if all_active { theme::on_accent() } else { theme::text() })
                .type_small()
                .cursor_pointer()
                .child("All")
                .on_click(cx.listener(|this, _, _, cx| this.apply_saved_filter(None, None, cx))),
        );
        for (ix, dlg) in self.saved_dialogs.iter().take(10).enumerate() {
            let active = self.saved_filter.as_ref().is_some_and(|(p, t)| p.as_deref() == Some(dlg.peer_id.as_str()) && t.is_none());
            let pid = dlg.peer_id.clone();
            let label = if dlg.title.is_empty() { dlg.peer_id.clone() } else { dlg.title.clone() };
            chips = chips.child(
                div()
                    .id(("saved-dlg", ix))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(if active { theme::accent() } else { theme::hover() })
                    .text_color(if active { theme::on_accent() } else { theme::text() })
                    .type_small()
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.apply_saved_filter(Some(pid.clone()), None, cx);
                    })),
            );
        }
        for (ix, tag) in self.saved_tags.iter().take(10).enumerate() {
            let active = self.saved_filter.as_ref().is_some_and(|(p, t)| p.is_none() && t.as_deref() == Some(tag.emoji.as_str()));
            let emoji = tag.emoji.clone();
            let label = if tag.title.is_empty() {
                format!("{} {}", tag.emoji, tag.count)
            } else {
                format!("{} {} ({})", tag.emoji, tag.title, tag.count)
            };
            chips = chips.child(
                div()
                    .id(("saved-tag", ix))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(if active { theme::accent() } else { theme::hover() })
                    .text_color(if active { theme::on_accent() } else { theme::text() })
                    .type_small()
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.apply_saved_filter(None, Some(emoji.clone()), cx);
                    })),
            );
        }
        let mut bar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .px(px(12.))
            .py(px(6.))
            .bg(theme::raised())
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(chips);
        if let Some(filtered) = &self.saved_filtered {
            let summary = if filtered.is_empty() {
                "No saved messages match this filter.".to_owned()
            } else {
                let first = filtered
                    .iter()
                    .take(3)
                    .map(|m| m.text.chars().take(60).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" · ");
                format!("{} saved: {first}", filtered.len())
            };
            bar = bar.child(div().type_small().text_color(theme::text_soft()).truncate().child(summary));
        }
        Some(bar)
    }

    /// Validates and submits the active bot/app interaction modal (`Request::ModalSubmit`).
    pub(super) fn submit_modal(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.interaction_modal.as_mut() else {
            return;
        };
        if active.submitting {
            return;
        }
        let mut values = Vec::with_capacity(active.modal.fields.len());
        for (field, input) in active.modal.fields.iter().zip(&active.inputs) {
            let text = input.read(cx).text().to_owned();
            let count = text.chars().count();
            if field.required && text.trim().is_empty() {
                active.error = Some(format!("{} is required.", field.label));
                cx.notify();
                return;
            }
            if let Some(min) = field.min_length {
                if (!text.is_empty() || field.required) && count < min as usize {
                    active.error = Some(format!("{} must be at least {min} characters.", field.label));
                    cx.notify();
                    return;
                }
            }
            if let Some(max) = field.max_length {
                if count > max as usize {
                    active.error = Some(format!("{} must be at most {max} characters.", field.label));
                    cx.notify();
                    return;
                }
            }
            values.push((field.custom_id.clone(), text));
        }
        active.error = None;
        active.submitting = true;
        let request = Request::ModalSubmit {
            conversation: active.conversation.clone(),
            modal_id: active.modal.id.clone(),
            application_id: active.modal.application_id.clone(),
            custom_id: active.modal.custom_id.clone(),
            values,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(Response::Ok) => {
                        this.interaction_modal = None;
                    }
                    Ok(Response::Error { detail, .. }) => {
                        if let Some(active) = this.interaction_modal.as_mut() {
                            active.submitting = false;
                            active.error = Some(detail);
                        }
                    }
                    Err(err) => {
                        if let Some(active) = this.interaction_modal.as_mut() {
                            active.submitting = false;
                            active.error = Some(crate::client::trouble_words("submit form", &err));
                        }
                    }
                    _ => {
                        this.interaction_modal = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn live_call_row(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.thread {
            return None;
        }
        let open = self.open.as_ref()?;
        let (key, occupants) = self.shell.upgrade().and_then(|shell| {
            let shell = shell.read(cx);
            let conv = shell.snapshot.as_ref()?.open.as_ref()?;
            if conv.key != open.key || conv.kind == protocol::ConversationKind::Voice {
                return None;
            }
            let mine = shell.snapshot.as_ref()?.call.as_ref().is_some_and(|c| c.conversation == conv.key);
            if mine {
                return None;
            }
            let occ = conv.occupants.as_ref().filter(|o| o.total > 0)?;
            Some((conv.key.clone(), occ.clone()))
        })?;
        let names: Vec<&str> = occupants.people.iter().take(3).map(|p| p.name.as_str()).collect();
        let summary = if names.is_empty() {
            format!("Call in progress · {}", occupants.total)
        } else {
            format!("Call in progress · {}", names.join(", "))
        };
        let shell = self.shell.clone();
        Some(
            div()
                .absolute()
                .left(px(self.metrics.inset))
                .right(px(self.metrics.inset))
                .bottom(px(10.))
                .h(px(40.))
                .px(px(12.))
                .rounded(px(theme::R_CARD))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::border())
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .size(px(26.))
                        .rounded_full()
                        .bg(theme::accent_soft())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::gui::components::glyph("icons/phone.svg", 13., theme::accent())),
                )
                .child(div().flex_1().min_w_0().truncate().type_small().text_color(theme::text()).child(summary))
                .child(
                    div()
                        .id("live-call-join")
                        .flex_none()
                        .h(px(26.))
                        .px(px(12.))
                        .rounded_full()
                        .bg(theme::accent())
                        .text_color(theme::on_accent())
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .type_micro()
                        .font_weight(theme::SEMIBOLD)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let _ = shell.update(cx, |shell, cx| shell.call_start(key.clone(), cx));
                        }))
                        .child("Join"),
                ),
        )
    }
    fn is_slack_app(&self) -> bool {
        !self.thread
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.key.network == protocol::Network::Slack && open.bot)
    }

    fn select_app_tab(&mut self, tab: AppTab, cx: &mut Context<Self>) {
        self.app_tab = tab;
        if tab != AppTab::Messages {
            self.load_app_view(tab == AppTab::Home, cx);
        }
        cx.notify();
    }

    fn load_app_view(&mut self, include_home: bool, cx: &mut Context<Self>) {
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        let have = self
            .app_view
            .as_ref()
            .is_some_and(|(k, v)| k == &conversation && (!include_home || v.home.is_some()));
        if have || self.app_loading {
            return;
        }
        self.app_loading = true;
        self.app_error = None;
        let expected = conversation.clone();
        cx.spawn(async move |pane, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    ask(&Request::AppView {
                        conversation,
                        include_home,
                    })
                })
                .await;
            let _ = pane.update(cx, |this, cx| {
                if this.key() != Some(&expected) {
                    return;
                }
                this.app_loading = false;
                match answer {
                    Ok(Response::AppView { view }) => {
                        this.app_view = Some((expected, *view));
                        this.app_error = None;
                    }
                    Ok(Response::Error { detail, .. }) => {
                        this.app_error = Some(detail);
                    }
                    Ok(_) => {
                        this.app_error = Some(crate::client::OTHER_ANSWER.to_owned());
                    }
                    Err(err) => {
                        this.app_error = Some(crate::client::trouble_words("load app", &err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn app_tabs_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.is_slack_app() {
            return None;
        }
        let open = self.open.as_ref()?;
        let team_id = open.key.account.clone();
        let channel_id = open.key.id.clone();
        let open_url = self
            .app_view
            .as_ref()
            .filter(|(k, _)| k == &open.key)
            .map(|(_, v)| v.profile.open_url.clone())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| format!("https://app.slack.com/client/{team_id}/{channel_id}"));
        let dir_url = self
            .app_view
            .as_ref()
            .filter(|(k, _)| k == &open.key)
            .map(|(_, v)| v.profile.directory_url.clone())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| format!("https://app.slack.com/apps/{team_id}"));
        let tabs = [
            (AppTab::Home, "Home"),
            (AppTab::Messages, "Messages"),
            (AppTab::About, "About"),
        ];
        let mut bar = super::components::segmented();
        for (ix, (tab, label)) in tabs.into_iter().enumerate() {
            bar = bar.child(
                super::components::segment(("slack-app-tab", ix), label, self.app_tab == tab)
                    .px(px(10.))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_app_tab(tab, cx))),
            );
        }
        Some(
            div()
                .flex_none()
                .px(px(theme::PANE_PAD))
                .py(px(6.))
                .border_b_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(bar)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            super::components::button_with_icon(
                                "slack-app-open",
                                super::components::ButtonKind::Secondary,
                                "icons/external-link.svg",
                                "Open app in Slack",
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_external_link(&open_url, window, cx);
                            })),
                        )
                        .child(
                            super::components::button_with_icon(
                                "slack-app-directory",
                                super::components::ButtonKind::Secondary,
                                "icons/plus.svg",
                                "Add apps",
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_external_link(&dir_url, window, cx);
                            })),
                        ),
                ),
        )
    }

    fn app_tab_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut body = div()
            .id("slack-app-tab-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(theme::PANE_PAD))
            .flex()
            .flex_col()
            .gap(px(12.));
        if self.app_loading {
            return body
                .child(div().type_ui().text_color(theme::text_soft()).child("Loading app…"))
                .into_any_element();
        }
        if let Some(err) = &self.app_error {
            return body
                .child(div().type_ui().text_color(theme::danger()).child(err.clone()))
                .into_any_element();
        }
        let Some((conv_key, view)) = self.app_view.as_ref().filter(|(k, _)| Some(k) == self.key()) else {
            return body
                .child(div().type_ui().text_color(theme::text_soft()).child("No app details loaded yet."))
                .into_any_element();
        };
        match self.app_tab {
            AppTab::Home => {
                if let Some(home) = &view.home {
                    if !home.text.trim().is_empty() {
                        body = body.child(div().type_body().text_color(theme::text()).child(home.text.clone()));
                    } else if !home.empty_reason.is_empty() {
                        body = body.child(div().type_ui().text_color(theme::text_soft()).child(home.empty_reason.clone()));
                    }
                    if !home.controls.is_empty() {
                        let mut row = div().flex().flex_wrap().gap(px(8.));
                        for (ix, ctrl) in home.controls.iter().enumerate() {
                            if let protocol::ControlKind::Button { url, style } = &ctrl.kind {
                                let btn_kind = match style {
                                    protocol::ControlStyle::Primary => super::components::ButtonKind::Primary,
                                    protocol::ControlStyle::Danger => super::components::ButtonKind::Danger,
                                    protocol::ControlStyle::Success => super::components::ButtonKind::Primary,
                                    protocol::ControlStyle::Default => super::components::ButtonKind::Secondary,
                                };
                                let label = if ctrl.label.is_empty() { "Action".to_owned() } else { ctrl.label.clone() };
                                if !url.is_empty() {
                                    let link = url.clone();
                                    row = row.child(
                                        super::components::button_with_icon(
                                            ("app-home-btn", ix),
                                            btn_kind,
                                            "icons/external-link.svg",
                                            label,
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_external_link(&link, window, cx);
                                        })),
                                    );
                                } else {
                                    let conv = conv_key.clone();
                                    let msg_id = format!("view:{}", home.view_id);
                                    let block_id = ctrl.block_id.clone();
                                    let action_id = ctrl.action_id.clone();
                                    let value = ctrl.value.clone();
                                    row = row.child(
                                        super::components::button(("app-home-btn", ix), btn_kind, label).on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.execute_control(
                                                    conv.clone(),
                                                    msg_id.clone(),
                                                    block_id.clone(),
                                                    action_id.clone(),
                                                    "button".to_owned(),
                                                    value.clone(),
                                                    Vec::new(),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    );
                                }
                            }
                        }
                        body = body.child(row);
                    }
                } else {
                    body = body.child(div().type_ui().text_color(theme::text_soft()).child("No Home view published."));
                }
            }
            AppTab::About => {
                let p = &view.profile;
                body = body.child(div().type_title().text_color(theme::text()).child(p.name.clone()));
                if !p.description.is_empty() {
                    body = body.child(div().type_ui().text_color(theme::text_soft()).child(p.description.clone()));
                }
                if !p.installed_by.is_empty() {
                    body = body.child(
                        div()
                            .type_small()
                            .text_color(theme::text_faint())
                            .child(format!("Installed by {}", p.installed_by)),
                    );
                }
                if !p.commands.is_empty() {
                    body = body.child(div().type_micro().text_color(theme::text_faint()).child("SLASH COMMANDS"));
                    for cmd in &p.commands {
                        let line = if cmd.usage.is_empty() {
                            cmd.command.clone()
                        } else {
                            format!("{} {}", cmd.command, cmd.usage)
                        };
                        body = body.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(line))
                                .when(!cmd.description.is_empty(), |el| {
                                    el.child(div().type_small().text_color(theme::text_soft()).child(cmd.description.clone()))
                                }),
                        );
                    }
                }
                if !p.links.is_empty() {
                    body = body.child(div().type_micro().text_color(theme::text_faint()).child("LINKS"));
                    let mut links_row = div().flex().flex_wrap().gap(px(6.));
                    for (ix, link) in p.links.iter().enumerate() {
                        let url = link.url.clone();
                        links_row = links_row.child(
                            super::components::button_with_icon(
                                ("app-about-link", ix),
                                super::components::ButtonKind::Secondary,
                                "icons/external-link.svg",
                                link.label.clone(),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_external_link(&url, window, cx);
                            })),
                        );
                    }
                    body = body.child(links_row);
                }
            }
            AppTab::Messages => {}
        }
        body.into_any_element()
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.images_stale {
            self.images_stale = false;
            self.images.clear(window, cx);
        }
        if self.watching_window.is_none() {
            // Coming back to the window is when what is on screen becomes read; leaving it
            // ends that at once, not at the next draw.
            self.watching_window = Some(cx.observe_window_activation(window, |this, window, cx| {
                this.active = window.is_window_active();
                cx.notify();
            }));
        }
        for path in std::mem::take(&mut self.replaced) {
            self.images.replaced(&path, window, cx);
        }
        for (path, edge) in std::mem::take(&mut self.let_go) {
            self.images.let_go(&path, edge, window, cx);
        }
        self.images.begin_frame();
        self.settle_hold(window, cx);
        self.settle_land(window, cx);
        if std::mem::take(&mut self.wants_focus) {
            window.focus(&self.focus, cx);
        }
        if self.is_gated() {
            return self.age_gate_view(cx);
        }
        self.round_stamp = self.round_stamp.wrapping_add(1);
        self.seen.borrow_mut().clear();
        let this = cx.entity();
        let rows = gpui::list(self.list.clone(), cx.processor(|this: &mut Pane, ix: usize, window, cx| this.row(ix, window, cx)))
            .size_full();
        let after = gpui::canvas(
            |_, _, _| (),
            move |_, (), window, cx| {
                this.update(cx, |this, cx| this.after_layout(window, cx));
                let pane = this.clone();
                // Track geometry before hit testing/occlusion can hide the pane behind
                // its own toolbar. Do not consume the event: buttons still receive it.
                window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        pane.update(cx, |this, cx| this.point_at(event.position, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0();
        let app_bar = self.app_tabs_bar(cx);
        let pin_bar = self.pin_bar(cx);
        let showing_app_tab = self.is_slack_app() && self.app_tab != AppTab::Messages;
        let ephemeral_strip = (!self.ephemeral.is_empty() && !showing_app_tab).then(|| {
            let mut list = div()
                .flex_none()
                .px(px(theme::PANE_PAD))
                .py(px(6.))
                .border_t_1()
                .border_color(theme::hairline())
                .bg(theme::raised())
                .flex()
                .flex_col()
                .gap(px(6.));
            for (ix, msg) in self.ephemeral.iter().enumerate() {
                let id = msg.id.clone();
                let who = if msg.sender.name.is_empty() { "Slackbot".to_owned() } else { msg.sender.name.clone() };
                list = list.child(
                    div()
                        .id(("ephemeral-msg", ix))
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(who))
                                        .child(div().type_micro().text_color(theme::text_faint()).child("Only visible to you")),
                                )
                                .child(div().type_ui().text_color(theme::text_soft()).child(msg.text.clone())),
                        )
                        .child(
                            div()
                                .id(("ephemeral-dismiss", ix))
                                .type_micro()
                                .text_color(theme::text_faint())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ephemeral.retain(|m| m.id != id);
                                    cx.notify();
                                }))
                                .child("Dismiss"),
                        ),
                );
            }
            list
        });
        div()
            .id("messages")
            .size_full()
            .flex()
            .flex_col()
            // A phone leaves 11 pt under the last row so it never touches the composer and the
            // last message's words sit 12–20 pt above the card.
            .when(self.metrics == Metrics::PHONE, |body| body.pb(px(11.)))
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    // A press on text starts a selection; one anywhere else ends it.
                    if !std::mem::take(&mut this.on_text) && this.selection.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if release_text_drag(&mut this.dragging, true) {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if release_text_drag(&mut this.dragging, false) {
                        cx.notify();
                    }
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if !*hovered {
                    // Occlusion is not a pointer exit; recheck the row/toolbar bounds.
                    this.point_at(window.mouse_position(), cx);
                }
            }))
            .children(app_bar)
            .children(pin_bar)
            .child(if showing_app_tab {
                self.app_tab_body(cx)
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(crate::gui::components::scroll_frame("message-scroll-frame", self.list.clone(), rows).keyboard(false))
                    .child(after)
                    .children(self.saved_filter_bar(cx))
                    .children(self.live_call_row(cx))
                    .children(self.history_banner(cx))
                    .children(self.unread_jump_buttons(cx))
                    .children(self.away.then(|| self.jump_button(cx)))
                    .children(self.selection_bar(cx))
                    .into_any_element()
            })
            .children(self.tag_popover_layer(cx))
            .children(self.action_layers(window, cx))
            .children(ephemeral_strip)
            .children(self.viewer_layer(window, cx))
            .children(self.text_viewer_layer(window, cx))
    }
}

impl Pane {
    /// The latest reply keyboard while `key` is the open conversation.
    /// A selective one stays hidden unless it mentions the reader or answers one of theirs,
    /// and an older keyboard is not used in its place.
    /// An empty keyboard is a hide: it clears what is showing. A message with no reply
    /// markup is skipped, so the keyboard before it stays. A selective hide that is not
    /// for the reader is skipped the same way.
    pub(super) fn reply_keyboard(&self, key: &Key) -> Option<(String, protocol::ReplyKeyboard)> {
        let open = self.open.as_ref()?;
        if &open.key != key {
            return None;
        }
        for message in self.window.messages.iter().rev() {
            let Some(board) = message.keyboard.as_ref() else {
                continue;
            };
            let answers_me = message.reply_to.as_ref().is_some_and(|reply| {
                self.window.messages.iter().any(|held| held.id == reply.id && held.outgoing)
            });
            let for_me = message.mentions_me || answers_me;
            if board.rows.is_empty() {
                if board.selective && !for_me {
                    continue;
                }
                return None;
            }
            if board.selective && !for_me {
                return None;
            }
            return Some((message.id.clone(), board.clone()));
        }
        None
    }

    /// Start belongs on an empty bot chat. A thread, or a window that still has older history, has none.
    pub(super) fn bot_can_start(&self, key: &Key) -> bool {
        self.open.as_ref().is_some_and(|open| &open.key == key) && self.window.messages.is_empty() && !self.window.older && !self.thread
    }
}

impl Shell {
    pub(in crate::gui) fn pane(&self, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = self
            .conversation_state
            .pane
            .get_or_init(|| {
                let shell = cx.weak_entity();
                let metrics = if super::phone::active() { Metrics::PHONE } else { Metrics::FULL };
                cx.new(|cx| Pane::new(metrics, false, shell, cx))
            })
            .clone();
        self.subscribe_pane(&pane, cx);
        self.follow_changes(cx);
        pane
    }

    /// Opens a web link the way a message link does: parse it, then confirm or strip tracking.
    pub(in crate::gui) fn open_web_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane(cx);
        pane.update(cx, |pane, cx| pane.open_external_link(url, window, cx));
    }
    fn forum_board(&self, cx: &mut Context<Self>) -> Entity<forum::Board> {
        self.conversation_state
            .forum
            .get_or_init(|| {
                let shell = cx.weak_entity();
                cx.new(|cx| forum::Board::new(shell, cx))
            })
            .clone()
    }
    /// The reply keyboard under the composer, from the open pane.
    pub(in crate::gui) fn bot_reply_keyboard(&self, key: &Key, cx: &gpui::App) -> Option<(String, protocol::ReplyKeyboard)> {
        self.conversation_state.pane.get()?.read(cx).reply_keyboard(key)
    }

    /// Whether the open pane should offer Start for this bot chat.
    pub(in crate::gui) fn bot_can_start(&self, key: &Key, cx: &gpui::App) -> bool {
        self.conversation_state.pane.get().is_some_and(|pane| pane.read(cx).bot_can_start(key))
    }

    /// `messages.startBot`. An empty `start_param` is the Start button; a deep link passes its payload.
    pub(in crate::gui) fn start_bot(&mut self, conversation: Key, start_param: String, cx: &mut Context<Self>) {
        super::people::ask_later(cx, Request::StartBot { conversation, start_param }, |_, _| {});
    }

    /// A bot menu or keyboard URL, through the pane's link confirm.
    pub(in crate::gui) fn open_pane_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.conversation_state.pane.get().cloned() else {
            return;
        };
        let url = url.to_owned();
        pane.update(cx, |pane, cx| pane.open_external_link(&url, window, cx));
    }

    /// My own id on the account a conversation belongs to.
    fn me(&self, conversation: &Key) -> String {
        let account = conversation.account();
        let accounts = self.snapshot.iter().flat_map(|snapshot| &snapshot.accounts);
        accounts.filter(|status| status.key == account).map(|status| status.me.clone()).next().unwrap_or_default()
    }

    /// Whether an age-restricted conversation is still behind this session's warning gate.
    pub(in crate::gui) fn age_gated(&self, conversation: &Conversation) -> bool {
        conversation.age_restricted && !self.conversation_state.passed_age_gate.contains(&conversation.key)
    }

    /// Age-gate flags for a thread pane. The thread row wins; otherwise the open parent
    /// channel's mark is used, and the session check is the same one `show` applies.
    fn thread_age_gate(&self, thread: &Key, parent: &Key) -> (bool, bool) {
        if let Some(row) = self.sidebar_state.find(&self.trees, thread) {
            return (row.age_restricted, self.age_gated(&row));
        }
        let age_restricted = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.open.as_ref())
            .is_some_and(|open| &open.key == parent && open.age_restricted);
        (age_restricted, age_restricted && !self.age_gate_passed(thread))
    }

    /// Whether the user already clicked Continue for `key` during this session.
    pub(in crate::gui) fn age_gate_passed(&self, key: &Key) -> bool {
        self.conversation_state.passed_age_gate.contains(key)
    }

    /// Remembers that the user confirmed the age warning for `key` for this session.
    pub(in crate::gui) fn pass_age_gate(&mut self, key: Key, cx: &mut Context<Self>) {
        self.conversation_state.passed_age_gate.insert(key.clone());
        self.info_age_gate_passed(&key, cx);
        cx.notify();
    }

    /// Leaves an age-restricted conversation when the user clicks Nope.
    pub(in crate::gui) fn leave_age_gated(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        if let Some(snapshot) = &mut self.snapshot {
            snapshot.open = None;
        }
        cx.notify();
    }

    /// Opens the Follow announcement channel picker for `open`, listing channels across this
    /// account's servers where the user has Manage Webhooks.
    pub(super) fn open_follow(&mut self, open: &Conversation, window: &mut Window, cx: &mut Context<Self>) {
        let account = open.key.account();
        let spaces: Vec<protocol::Space> = self
            .spaces
            .iter()
            .filter(|space| space.key.account() == account)
            .cloned()
            .collect();
        let source = open.key.clone();
        let source_title = open.title.clone();
        let pane = self.pane(cx);
        pane.update(cx, |pane, cx| pane.open_follow(source, source_title, spaces, window, cx));
    }

    /// The open conversation was marked unread from a row or the menu bar. The pane keeps the
    /// divider and does not read the messages already on screen.
    pub(super) fn hold_open_unread(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get().cloned() {
            pane.update(cx, |pane, cx| pane.hold_unread(cx));
        }
    }

    /// Routes external HTTP/HTTPS link opens through `Pane::open_external_link` (which validates
    /// via `Destination::parse`, enforces http/https only, strips tracking, and honors link
    /// confirmation preferences).
    pub(in crate::gui) fn open_external_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get().cloned() {
            pane.update(cx, |pane, cx| pane.open_external_link(url, window, cx));
            return;
        }
        if let Some(dest) = protocol::link::Destination::parse(url) {
            let strip = crate::gui::app::prefs::choices().strip_tracking;
            cx.open_url(&dest.target(strip));
        }
    }

    /// Back on a phone: what a pane of messages has over it (the viewer, a message's sheet, the
    /// picker, the delete question) closes first, the thread's before the conversation's.
    pub(in crate::gui) fn panes_back(&mut self, cx: &mut Context<Self>) -> bool {
        let thread = self.conversation_state.thread.borrow().clone().filter(|_| self.layout_state.right.is_some());
        let main = self.conversation_state.pane.get().cloned().filter(|_| self.open.is_some());
        thread.into_iter().chain(main).any(|pane| pane.update(cx, |pane, cx| pane.phone_back(cx)))
    }

    /// The main pane, its thread, and any conversation window. Not the phone's back stack.
    fn message_panes(&self) -> Vec<Entity<Pane>> {
        let state = &self.conversation_state;
        let mut panes: Vec<_> = state.pane.get().cloned().into_iter().chain(state.thread.borrow().clone()).collect();
        panes.extend(state.windows.panes());
        panes
    }

    /// Draws the panes again: a player in them moved on. Only marks them, so it is safe from
    /// inside a pane's own handler, where the pane cannot be updated.
    pub(super) fn conversation_redraw(&mut self, cx: &mut Context<Self>) {
        let panes: Vec<gpui::EntityId> = self.message_panes().iter().map(Entity::entity_id).collect();
        let app: &mut gpui::App = cx;
        for pane in panes {
            app.notify(pane);
        }
        self.composer_playback_redraw(cx);
    }

    /// Offered every change chatd reports, after `Shell` has applied its own part.
    pub(super) fn conversation_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        for pane in self.message_panes() {
            pane.update(cx, |pane, cx| pane.changed(change, cx));
        }
        if let Some(board) = self.conversation_state.forum.get().cloned() {
            board.update(cx, |board, cx| board.media_changed(change, cx));
        }
    }

    /// chatd no longer knows what changed: everything held is read again.
    #[allow(dead_code)]
    pub(super) fn conversation_reset(&mut self, cx: &mut Context<Self>) {
        for pane in self.message_panes() {
            pane.update(cx, |pane, cx| pane.refresh(cx));
        }
    }

    /// Follows the newest message, for after the user sends one.
    #[allow(dead_code)]
    pub(super) fn conversation_to_bottom(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get() {
            pane.update(cx, |pane, cx| pane.go_to_bottom(cx));
        }
    }

    /// Copies the text of a message of the open conversation, for the message menu.
    #[allow(dead_code)]
    pub(super) fn copy_message(&self, message_id: &str, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get() {
            if let Some(text) = pane.read(cx).message_text(message_id) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    /// Opens the delete confirmation dialog for a message in the open conversation (used when
    /// editing a message down to empty text and zero attachments).
    pub(super) fn confirm_delete_message(&mut self, message_id: &str, cx: &mut Context<Self>) {
        if let Some(pane) = self.conversation_state.pane.get().cloned() {
            pane.update(cx, |pane, cx| pane.confirm_delete_by_id(message_id, cx));
        }
    }

    pub(super) fn conversation(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(open) = snapshot.open.as_ref() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(drags_window(div().h(px(TOP_BAR)).flex_none()))
                .child(empty_state(
                    "icons/message-circle.svg",
                    "Select a conversation",
                    "Telegram, Discord, Slack and Teams, in one list.",
                ));
        };
        #[cfg(feature = "gui-shot")]
        let staged = shot::instead(open);
        #[cfg(feature = "gui-shot")]
        let open = staged.as_ref().unwrap_or(open);
        let link = snapshot.account_link(&open.key);
        let forum_board = open.kind == ConversationKind::Forum && open.key.network == Network::Discord;
        let pane = (!forum_board).then(|| self.pane(cx));
        let board = forum_board.then(|| self.forum_board(cx));
        let me = self.me(&open.key);
        let can = snapshot.account(&open.key.account()).map(|status| status.can).unwrap_or_default();
        let picture = self.conversation_picture(open, cx);
        let gated = self.age_gated(open);
        // With no composer to type in, the messages take the keys after an open.
        let typing = open.special != Special::Service && !gated;
        if let Some(pane) = &pane {
            pane.update(cx, |pane, cx| pane.show(open, &me, can, picture, typing, gated, cx));
        }
        if let Some(board) = &board {
            board.update(cx, |board, cx| board.show(open, cx));
        }
        let keys = pane.clone();
        let phone = super::phone::active();
        let mut view = div().relative().size_full().flex().flex_col();
        // Paging keys reach the messages from the composer too.
        if let Some(keys) = keys.clone() {
            view = view.on_key_down(move |event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "pageup" | "pagedown") {
                    keys.update(cx, |pane, cx| pane.key_down(event, window, cx));
                }
            });
        }
        if phone {
            // The back gesture: a drag that began in the left 20 pt and passed 60 pops, as
            // iOS's edge swipe does (`01-ia-navigation.md` §5.2).
            view = view
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.phone_drag_back(f32::from(event.position.x), true, cx);
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.phone_release_edge()));
        }
        view.child(if phone {
            self.phone_strip(open, link, window, cx)
        } else {
            self.strip(open, link, window, cx)
        })
        .children((!phone && !forum_board).then(|| self.call_conversation_picture(snapshot, open, cx)).flatten())
        .child(match (board, pane) {
            (Some(board), _) => div().flex_1().min_h_0().child(board),
            (_, Some(pane)) => div().flex_1().min_h_0().child(pane),
            _ => div().flex_1(),
        })
        .children((!gated).then(|| match open.special {
            Special::Service => self.no_composer(open, cx),
            _ => self.composer(open, link, cx),
        }))
        .children(phone.then(|| self.phone_edge(cx)))
    }

    /// The network's own account (Telegram's service notifications) takes no messages: a line
    /// says so where the composer would be. The card and the sweep the composer hosts still run.
    fn no_composer(&self, open: &Conversation, cx: &mut Context<Self>) -> Div {
        let floating = self.floating(cx);
        self.sweep(cx);
        let network = open.key.network.label();
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
            .child(format!("Only {network} posts here."))
            .children(floating)
    }
}

/// My pending messages as they were first drawn: chatd may stamp one again while it goes,
/// and a row that moves or changes its time under the user is not where they left it. What
/// is no longer pending is let go.
fn held_in_place(messages: &[Rc<Message>], first: &mut HashMap<String, i64>) -> Vec<Rc<Message>> {
    first.retain(|client, _| messages.iter().any(|m| m.state == SendState::Pending && m.client_id.as_deref() == Some(client)));
    let mut moved = false;
    let mut held: Vec<Rc<Message>> = messages
        .iter()
        .map(|message| {
            let Some(client) = message.client_id.as_ref().filter(|_| message.state == SendState::Pending) else {
                return message.clone();
            };
            let sent = *first.entry(client.clone()).or_insert(message.sent);
            if sent == message.sent {
                return message.clone();
            }
            moved = true;
            Rc::new(Message { sent, ..(**message).clone() })
        })
        .collect();
    if moved {
        // The pinned time moves that one row. Everything else keeps its post order: messages
        // that share a stamp are not the same moment.
        held.sort_by_key(|message| message.rank());
    }
    held
}

/// Where the list's top goes so the "New" line at row `new` sits a third of the way down a
/// viewport `height` tall, given the height of everything from the line down (`below`, at
/// least until it passes the viewport) and each row's height above it. `None` when what is
/// unread fits: the list stays at the bottom.
fn landing(below: f32, height: f32, new: usize, mut tall: impl FnMut(usize) -> f32) -> Option<ListOffset> {
    if below <= height {
        return None;
    }
    let want = height / 3.;
    let (mut at, mut above) = (new, 0.);
    while at > 0 && above < want {
        at -= 1;
        above += tall(at);
    }
    let offset = if above >= want { above - want } else { 0. };
    Some(ListOffset { item_ix: at, offset_in_item: px(offset) })
}

/// A text drag ends when the button comes up. The release is often outside the message
/// pane. Hearing only the release that lands on the pane left the blue selection following
/// the pointer, and the row toolbar stayed hidden so the next clicks did nothing.
pub(crate) fn release_text_drag(dragging: &mut bool, _over_messages: bool) -> bool {
    std::mem::take(dragging)
}

/// A page that replaces the window. Older, newer, and arrivals extend it instead.
fn replaces_window(purpose: &Purpose) -> bool {
    matches!(
        purpose,
        Purpose::Open | Purpose::Newest | Purpose::Around | Purpose::Jump(_) | Purpose::Bottom | Purpose::Restore(..)
    )
}

/// The pinned bar of one conversation: the pins newest first and the one on show.
struct PinBar {
    key: Key,
    /// What the held page said was pinned when the pins were asked (`pins_seen`).
    seen: u64,
    pins: Vec<Rc<Message>>,
    at: usize,
}

/// A number for the set of pinned ids a page holds: a different number means the pins changed.
fn pins_seen<'a>(ids: impl Iterator<Item = &'a str>) -> u64 {
    ids.fold(0xcbf2_9ce4_8422_2325u64, |hash, id| id.bytes().chain([0]).fold(hash, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)))
}

/// The bar's small line. `at` counts from the newest pin, which Telegram numbers highest.
fn pin_bar_title(at: usize, total: usize) -> String {
    if total <= 1 {
        "Pinned message".to_owned()
    } else {
        format!("Pinned message #{} of {total}", total - at.min(total - 1))
    }
}

/// The pin the bar shows after a click on the one at `at`: the next older, then round again.
fn next_pin(at: usize, total: usize) -> usize {
    if total == 0 { 0 } else { (at + 1) % total }
}

/// `asked` is the read's number. `newest` is the latest replacing read started since.
/// A page asked earlier must not land over a later one. The current page still lands,
/// including an empty newest page, so a store that is really empty still clears the pane.
fn stale_page(purpose: &Purpose, asked: u64, newest: u64) -> bool {
    replaces_window(purpose) && asked < newest
}

/// The lab's stale-page driver. A late empty newest page does not clear a newer read.
pub(crate) fn prove_selection_clamp() -> Result<(), &'static str> {
    text::prove_selection_clamp()
}

pub(crate) fn prove_stale_page() -> Result<(), &'static str> {
    if stale_page(&Purpose::Newest, 1, 2)
        && stale_page(&Purpose::Open, 1, 2)
        && !stale_page(&Purpose::Newest, 2, 2)
        && !stale_page(&Purpose::Older, 1, 2)
    {
        Ok(())
    } else {
        Err("a late newest page would replace a newer read")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_bar_counts_from_the_newest_pin_and_goes_round() {
        assert_eq!(pin_bar_title(0, 1), "Pinned message");
        assert_eq!(pin_bar_title(0, 3), "Pinned message #3 of 3");
        assert_eq!(pin_bar_title(2, 3), "Pinned message #1 of 3");
        assert_eq!([next_pin(0, 3), next_pin(2, 3), next_pin(0, 1), next_pin(0, 0)], [1, 0, 0, 0]);
        // A pin made or taken elsewhere changes what the page holds, and so the number.
        assert_eq!(pins_seen(["12", "20"].into_iter()), pins_seen(["12", "20"].into_iter()));
        assert_ne!(pins_seen(["12", "20"].into_iter()), pins_seen(["12"].into_iter()));
        assert_ne!(pins_seen(["1", "22"].into_iter()), pins_seen(["12", "2"].into_iter()));
    }

    /// The open read and the refill can both be in flight. The one asked earlier must not land
    /// last and put an empty or blank page over the rows the later read already showed. The
    /// current newest page still lands, so a store that is really empty still clears the pane.
    #[test]
    fn the_edited_mark_names_the_time_and_an_earlier_wording_names_its_own() {
        assert_eq!(edited_tip(None), "Edited");
        let at = 1_700_000_000;
        assert_eq!(edited_tip(Some(at)), format!("Edited {}", crate::gui::time::tooltip(at)));
        assert_eq!(revision_line(at, "  hello  "), format!("{} · hello", crate::gui::time::clock(at)));
        assert_eq!(revision_line(at, "   "), format!("{} · (empty)", crate::gui::time::clock(at)));
    }

    #[test]
    fn a_late_newest_page_does_not_replace_a_newer_read() {
        assert!(stale_page(&Purpose::Newest, 1, 2));
        assert!(stale_page(&Purpose::Open, 1, 2));
        assert!(stale_page(&Purpose::Around, 1, 2));
        assert!(stale_page(&Purpose::Restore("m".into(), 0.), 1, 2));
        assert!(!stale_page(&Purpose::Newest, 2, 2));
        assert!(!stale_page(&Purpose::Older, 1, 2));
        assert!(!stale_page(&Purpose::Arrivals, 1, 2));
    }

    #[test]
    fn a_message_that_arrives_while_reading_at_the_bottom_gets_no_new_line() {
        // Open, at the newest message, window active, no line up: the marker follows.
        assert!(reads_as_it_lands(true, true, true, false));
        // The first page is what places the line. It never moves the marker past the unread.
        assert!(!reads_as_it_lands(false, true, true, false));
        // Scrolled up, or the window is not active: what arrives is unread, under a line.
        assert!(!reads_as_it_lands(true, false, true, false));
        assert!(!reads_as_it_lands(true, true, false, false));
        // A line from the opening stays put until the user leaves.
        assert!(!reads_as_it_lands(true, true, true, true));
    }

    #[test]
    fn a_failed_page_says_which_one_and_a_newer_failure_stops_the_retry() {
        assert_eq!(
            history_failure(&Purpose::Newer, "offline"),
            "Couldn't load newer messages. offline"
        );
        assert_eq!(
            history_failure(&Purpose::Jump("m".into()), "gone"),
            "Couldn't load that message. gone"
        );
        assert_eq!(
            history_failure(&Purpose::Bottom, "slow"),
            "Couldn't load the latest messages. slow"
        );
        assert_eq!(
            history_failure(&Purpose::Older, "nope"),
            "Couldn't load older messages. nope"
        );
        assert!(Purpose::Jump("m".into()) != Purpose::Newer);
    }

    #[test]
    fn a_pending_row_keeps_its_time_and_place_until_it_is_sent() {
        let said = |id: &str, sent: i64| Rc::new(Message::plain(id, "Ines", id, sent, false));
        let mut mine = Message::plain("p1", "Me", "hi", 100, true);
        mine.state = SendState::Pending;
        mine.client_id = Some("c1".into());
        let mut first = HashMap::new();
        let held = held_in_place(&[said("a", 90), Rc::new(mine.clone()), said("b", 110)], &mut first);
        assert_eq!(held.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "p1", "b"]);
        // Stamped again later: drawn where and when it was.
        mine.sent = 130;
        let held = held_in_place(&[said("a", 90), said("b", 110), Rc::new(mine.clone())], &mut first);
        assert_eq!(held.iter().map(|m| (m.id.as_str(), m.sent)).collect::<Vec<_>>(), [("a", 90), ("p1", 100), ("b", 110)]);
        // Sent: its own time from then on, and nothing is held for it.
        mine.state = SendState::Sent;
        let held = held_in_place(&[said("a", 90), said("b", 110), Rc::new(mine)], &mut first);
        assert_eq!(held.last().map(|m| m.sent), Some(130));
        assert!(first.is_empty());
    }

    #[test]
    fn unread_that_fits_opens_at_the_bottom_and_more_puts_the_line_a_third_down() {
        let place = |at: Option<ListOffset>| at.map(|at| (at.item_ix, f32::from(at.offset_in_item)));
        // A 600 tall viewport: rows of 40 above the line at row 20.
        assert_eq!(place(landing(500., 600., 20, |_| 40.)), None);
        // 200 above the line: five rows of 40, so row 15 at the top with nothing cut off.
        assert_eq!(place(landing(900., 600., 20, |_| 40.)), Some((15, 0.)));
        // Rows that do not divide evenly: the top one is cut so the line still lands at 200.
        let (ix, offset) = place(landing(900., 600., 20, |_| 47.)).unwrap();
        assert_eq!(ix, 15);
        assert!((offset - (47. * 5. - 200.)).abs() < 0.01);
        // Too little history above: the top of what is held.
        assert_eq!(place(landing(900., 600., 2, |_| 40.)), Some((0, 0.)));
    }

    #[test]
    fn nothing_is_marked_read_from_a_screenshot_an_inactive_window_or_off_screen() {
        // The one case that tells the network: a real, active window with the message on show.
        assert!(may_mark_read(false, true, true, true));
        // A build that takes screenshots never does, however the window looks to it.
        for (active, open, on_screen) in [(true, true, true), (false, true, true), (true, false, true), (true, true, false)] {
            assert!(!may_mark_read(true, active, open, on_screen));
        }
        // A real window in the background, with no conversation, or scrolled away from it.
        assert!(!may_mark_read(false, false, true, true));
        assert!(!may_mark_read(false, true, false, true));
        assert!(!may_mark_read(false, true, true, false));
        // This build is the one the feature says it is.
        assert_eq!(SCREENSHOTS, cfg!(feature = "gui-shot"));
    }

    #[test]
    fn a_selection_covers_the_blocks_between_its_ends_whichever_way_it_was_dragged() {
        let dragged = |anchor, head| Selection { message: "m".into(), anchor, head: Some(head) };
        let down = dragged((1, 4), (3, 2));
        assert_eq!(down.within(0), None);
        assert_eq!(down.within(1), Some(4..usize::MAX));
        assert_eq!(down.within(2), Some(0..usize::MAX));
        assert_eq!(down.within(3), Some(0..2));
        assert_eq!(down.within(4), None);
        // Backwards is the same stretch.
        let up = dragged((3, 2), (1, 4));
        assert_eq!((up.within(1), up.within(3)), (Some(4..usize::MAX), Some(0..2)));
        // A press that has not moved selects nothing.
        let pressed = Selection { message: "m".into(), anchor: (1, 4), head: None };
        assert_eq!(pressed.within(1), None);
        assert_eq!(dragged((1, 4), (1, 4)).within(1), None);
    }

    #[test]
    fn a_release_outside_the_message_pane_ends_the_text_drag() {
        let mut dragging = true;
        assert!(
            release_text_drag(&mut dragging, false),
            "the button came up outside the pane"
        );
        assert!(!dragging);
    }

    #[test]
    fn age_restricted_channel_gates_until_passed_for_the_session() {
        let mut passed: HashSet<Key> = HashSet::new();
        let nsfw = Key { network: protocol::Network::Discord, account: "1".into(), id: "4101".into() };
        let normal = Key { network: protocol::Network::Discord, account: "1".into(), id: "4102".into() };
        let mut row = Conversation::new(nsfw.clone(), ConversationKind::Channel, "nsfw");
        row.age_restricted = true;
        let is_gated = |conv: &Conversation, passed: &HashSet<Key>| conv.age_restricted && !passed.contains(&conv.key);
        assert!(is_gated(&row, &passed));
        assert!(!is_gated(&Conversation::new(normal, ConversationKind::Channel, "general"), &passed));
        passed.insert(nsfw);
        assert!(!is_gated(&row, &passed));
    }
}
