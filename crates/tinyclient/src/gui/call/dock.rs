//! The call dock: one card at the bottom of the sidebar that is every state of a call. Ringing
//! out, ringing in, connecting, in a call and failed are the same card with different children.
//! Over a Tiny server's own column, whose user panel holds mute and deafen, a call that is up is
//! the compact "Voice connected" bar instead: where, how well, and Disconnect.

use super::health::{self, Grade};
use super::people::{self, ringed};
use super::security::Place;
use super::{
    ensure_daemon, ensure_keys, handoff_label, icon as glyph_in, in_call, open, Confirm, Ended,
    Menu, Subject,
};
use crate::gui::components::place::{anchor, placed, Layer, Place as OverlayPlace};
use crate::gui::components::{glyph, network_icon, tip, Avatar};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme::{self, Type};
use gpui::{
    canvas, deferred, div, prelude::*, px, AnyElement, Context, Div, FontWeight, MouseButton, Rgba, Stateful, Window,
};
use protocol::{
    Call, CallFailure, CallState, Content, ConversationKind, Direction, Key, Network, Request,
};

#[cfg(target_os = "macos")]
const SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
#[cfg(target_os = "windows")]
const SETTINGS: &str = "ms-settings:privacy-microphone";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const SETTINGS: &str = "";
/// Dock insets (8 + 8) plus the card's own padding (9 + 9). The name line is this much
/// narrower than the sidebar, and nothing else sits on it.
const CARD_INSET: f32 = 34.;

fn card() -> Div {
    div()
        .relative()
        .w_full()
        .flex_none()
        .rounded(px(12.))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::hairline())
        // 10 with the border.
        .p(px(9.))
        .flex()
        .flex_col()
        .gap(px(8.))
}

/// Width left for words inside the call card at a sidebar width.
fn card_inner(sidebar: f32) -> f32 {
    sidebar - CARD_INSET
}

/// How a 40 x 32 control looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Rest,
    /// My mic or audio is off.
    Off,
    /// Its menu is open.
    Open,
    /// Share or camera is on.
    On,
}

/// One control of the bar: 40 x 32, radius 8, a 16 px glyph. The card is `raised` already, so
/// a control at rest takes the white wash that reads as raised on it.
fn control(id: &'static str, icon: &'static str, look: Look, enabled: bool) -> Stateful<Div> {
    let (fill, color) = match look {
        Look::Rest => (theme::hover(), theme::text_soft()),
        Look::Off => (theme::danger_soft(), theme::danger()),
        Look::Open => (theme::selected(), theme::text()),
        Look::On => (theme::accent_soft(), theme::accent()),
    };
    // Only a live control at rest brightens its glyph under the pointer.
    let lit = (enabled && look == Look::Rest).then(|| (id, theme::text()));
    div()
        .id(id)
        .group(id)
        .role(gpui::Role::Button)
        .aria_disabled(!enabled)
        .flex_none()
        .w(px(40.))
        .h(px(32.))
        .rounded(px(8.))
        .when(enabled, |control| control.tab_index(0))
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .bg(fill)
        .text_color(color)
        .flex()
        .items_center()
        .justify_center()
        .child(glyph_in(icon, 16., color, lit))
        .map(|control| match (enabled, look) {
            (false, _) => control.opacity(0.4),
            (true, Look::Rest) => control
                .cursor_pointer()
                .hover(|style| style.bg(theme::selected()).text_color(theme::text())),
            (true, Look::On) => control.cursor_pointer().hover(|style| style.bg(theme::pressed())),
            (true, _) => control.cursor_pointer(),
        })
}

/// Our own encode of this picture is on.
fn encoding(call: &Call, content: Content) -> bool {
    call.video.iter().any(|stream| stream.direction == Direction::Encode && stream.content == content)
}

/// Leave, 48 x 32 at the right edge. It is live in every state that has it.
fn leave() -> Stateful<Div> {
    let id = "call-leave";
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label("Leave call")
        .group(id)
        .flex_none()
        .w(px(48.))
        .h(px(32.))
        .rounded(px(8.))
        .tab_index(0)
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .bg(theme::danger_soft())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(glyph_in("icons/phone-off.svg", 16., theme::danger(), Some((id, theme::on_accent()))))
        .hover(|style| style.bg(theme::danger_fill()))
        .tooltip(tip("Leave", Some("⌘⇧H")))
}

/// Subtitle line on the incoming call card (`#451`).
pub(crate) fn incoming_subtitle(call: &Call, kind: ConversationKind, from: &str, occupied: u32) -> String {
    match kind {
        ConversationKind::Direct if call.incoming_video => "Incoming video call".to_owned(),
        ConversationKind::Direct => "Incoming call".to_owned(),
        ConversationKind::Group if occupied > 0 => {
            format!("{from} is calling · {occupied} in the call")
        }
        _ => format!("{from} is calling"),
    }
}

/// Accept button label for an incoming call: native calls say "Accept"; handoff-only calls say
/// "Answer in <Network>" (`#451`).
pub(crate) fn incoming_accept_label(call: &Call, support: protocol::CallSupport) -> String {
    if support == protocol::CallSupport::Handoff {
        format!("Answer in {}", call.conversation.network.label())
    } else {
        "Accept".to_owned()
    }
}

/// A 32 px button with words: Cancel, Decline, Accept.
fn worded(id: &'static str, icon: Option<&'static str>, word: impl Into<gpui::SharedString>, fill: Rgba, color: Rgba) -> Stateful<Div> {
    let word: gpui::SharedString = word.into();
    // The red ones fill on hover, and their glyph goes white with the word.
    let lit = (color == theme::danger()).then(|| (id, theme::on_accent()));
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(word.clone())
        .group(id)
        .h(px(32.))
        .px(px(14.))
        .rounded(px(8.))
        .tab_index(0)
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .bg(fill)
        .text_color(color)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .type_ui()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .children(icon.map(|icon| glyph_in(icon, 14., color, lit)))
        .child(word)
}

/// A button of the failure card. As wide as its words, so a narrow sidebar wraps the row
/// instead of squeezing "Open System Settings" down to nothing.
fn plain(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .min_h(px(28.))
        .px(px(8.))
        .rounded(px(8.))
        .tab_index(0)
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .bg(theme::hover())
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .type_ui()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.bg(theme::selected()))
}

/// The way out to the official app, on a card: the same object as the header's hand-off pill.
/// No fill, an outline, the network's own icon, where it goes, and an arrow out. It never looks
/// like a button that calls from here.
fn outlined(id: &'static str, network: Network) -> Stateful<Div> {
    div()
        .id(id)
        .group(id)
        // Its own width: where it goes is never cut short.
        .flex_none()
        .h(px(28.))
        .px(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .type_ui()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_soft())
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
        .child(network_icon(network, 14.))
        .child(handoff_label(network))
        .child(glyph_in("icons/arrow-up-right.svg", 12., theme::text_faint(), None))
}

/// What a failure card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Act {
    Again(&'static str),
    Message,
    Handoff,
    Settings,
}

/// A failed call as the card says it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Failure {
    pub title: String,
    pub line: String,
    pub icon: &'static str,
    pub acts: Vec<Act>,
}

/// The first sentence of `text`, and what follows it.
fn sentence(text: &str) -> (&str, &str) {
    match text.split_once(". ") {
        Some((first, rest)) => (first, rest),
        None => (text.trim_end_matches('.'), ""),
    }
}

/// Every reason a call can fail for, as a title, one line that says what to do next, and the
/// buttons. chatd, the adapter and mediad send `detail` in plain words (nothing of theirs that
/// reaches here is a code or an error string), so it is the line when there is one.
pub(super) fn failure(call: &Call, subject: &Subject, reason: CallFailure, detail: &str) -> Failure {
    if call.conversation.network == Network::Tiny {
        return tiny_failure(call, subject, reason, detail);
    }
    let network = call.conversation.network.label();
    let alert = "icons/triangle-alert.svg";
    let said = |fallback: String| if detail.trim().is_empty() { fallback } else { detail.to_owned() };
    let card = |title: &str, line: String, acts: &[Act]| Failure { title: title.to_owned(), line, icon: alert, acts: acts.to_vec() };
    let was_up = call.joined != 0;
    match reason {
        // CONTRACT: `CallFailure::NoAnswer` covers a decline and a ring nobody took. The
        // adapter says which in `detail`; the card says either with the person's name.
        CallFailure::NoAnswer if detail.contains("declined") => Failure {
            title: "Declined".to_owned(),
            line: format!("{} declined the call.", subject.title),
            icon: "icons/phone-off.svg",
            acts: vec![Act::Message],
        },
        CallFailure::NoAnswer => Failure {
            title: "No answer".to_owned(),
            line: format!("{} didn't pick up.", subject.title),
            icon: "icons/phone-outgoing.svg",
            acts: vec![Act::Again("Call again"), Act::Message],
        },
        CallFailure::Refused if was_up => card(
            "You're out of the call",
            said(format!("{network} ended the voice connection.")),
            &[Act::Again("Rejoin"), Act::Handoff],
        ),
        CallFailure::Refused => card(
            "Couldn't connect",
            said(format!("{network} refused the voice connection.")),
            &[Act::Again("Try again"), Act::Handoff],
        ),
        CallFailure::Encryption => card(
            &format!("{network} needs a newer tinyclient for calls"),
            said(format!("{network} raised its encryption requirement. Update tinyclient, or take this call in {network}.")),
            &[Act::Handoff],
        ),
        CallFailure::Lost if was_up => card(
            "Connection lost",
            said(format!("The connection to {network} dropped and didn't come back. You've left the call.")),
            &[Act::Again("Rejoin"), Act::Handoff],
        ),
        CallFailure::Lost => card(
            "Couldn't connect",
            said(format!("No reply from {network} after 10 seconds.")),
            &[Act::Again("Try again"), Act::Handoff],
        ),
        // CONTRACT: `CallFailure::Audio` is one reason for a denied permission, a missing
        // microphone, a missing speaker and a device that would not start. They are told
        // apart by how mediad's sentence for each begins (`mediad/src/media.rs`).
        CallFailure::Audio if detail.contains("System Settings") => card(
            "tinyclient can't use the microphone",
            "Allow it in System Settings › Privacy & Security › Microphone, then call again.".to_owned(),
            &[Act::Settings],
        ),
        CallFailure::Audio if detail.starts_with("Plug in a microphone") => {
            card("No microphone found", detail.to_owned(), &[Act::Again("Try again")])
        }
        CallFailure::Audio if detail.starts_with("Plug in headphones") => {
            card("No speaker found", detail.to_owned(), &[Act::Again("Try again")])
        }
        CallFailure::Audio if detail.starts_with("The microphone") || detail.starts_with("The speaker") => {
            let (title, next) = sentence(detail);
            card(title, next.to_owned(), &[Act::Again("Try again")])
        }
        CallFailure::Audio => card(
            "Couldn't start audio",
            said("Check Microphone and speaker, then try again.".to_owned()),
            &[Act::Again("Try again")],
        ),
        CallFailure::Engine => card(
            "Couldn't connect",
            said("The call engine stopped.".to_owned()),
            &[Act::Again("Try again"), Act::Handoff],
        ),
    }
}

/// A Tiny server's call that ended in words. There is no official app to hand off to, and
/// what put me out says whether I may come back.
// CONTRACT: the sentences are chatd's (`adapters/tiny/live.rs`: KICKED, BANNED, TIMED_OUT,
// ROOM_GONE, ELSEWHERE, MEDIA_LOST, MEDIA_TIMEOUT); each is told apart by a phrase of it.
fn tiny_failure(call: &Call, subject: &Subject, reason: CallFailure, detail: &str) -> Failure {
    let alert = "icons/triangle-alert.svg";
    let card = |title: &str, icon, acts: &[Act]| Failure { title: title.to_owned(), line: detail.to_owned(), icon, acts: acts.to_vec() };
    let rejoin = Act::Again("Rejoin");
    match reason {
        _ if detail.contains("removed from the call") => card("Removed from the call", "icons/user-x.svg", &[rejoin]),
        _ if detail.contains("banned") => card("Banned from this server", "icons/ban.svg", &[]),
        _ if detail.contains("timed out") => card("Timed out", "icons/timer.svg", &[]),
        _ if detail.contains("deleted") => card(&format!("{} was deleted", subject.title), alert, &[]),
        _ if detail.contains("another device") => card("In a call on another device", "icons/phone-outgoing.svg", &[Act::Again("Join here")]),
        _ if detail.contains("removed from this server") => card("Removed from this server", "icons/user-x.svg", &[]),
        CallFailure::Lost if call.joined != 0 => Failure {
            title: "Connection lost".to_owned(),
            line: if detail.is_empty() { format!("The connection to {} dropped. You've left the call.", subject.title) } else { detail.to_owned() },
            icon: "icons/wifi-off.svg",
            acts: vec![rejoin],
        },
        CallFailure::Lost | CallFailure::Refused | CallFailure::Engine => Failure {
            title: "Couldn't connect".to_owned(),
            line: if detail.is_empty() { "The server didn't take the call. Try again in a moment.".to_owned() } else { detail.to_owned() },
            icon: alert,
            acts: vec![Act::Again("Try again")],
        },
        // The microphone and the rest read as on any network.
        _ => {
            let mut said = failure(&Call { conversation: Key { network: Network::Discord, ..call.conversation.clone() }, ..call.clone() }, subject, reason, detail);
            said.acts.retain(|act| *act != Act::Handoff);
            said
        }
    }
}

/// `12:04`, or `1:02:03` past the hour.
fn clock(seconds: i64) -> String {
    match seconds / 3600 {
        0 => format!("{}:{:02}", seconds / 60, seconds % 60),
        hours => format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
    }
}

/// A question the card asks before it leaves a call: the title, one line, and two equal
/// buttons, the safe one first.
struct Question {
    title: String,
    stay: String,
    go: &'static str,
}

/// Mirrors `layout::discord_overview_width`, which is private to that module.
fn discord_column(window: f32, sidebar: f32, hidden: bool, right: Option<f32>, server: bool) -> f32 {
    const OVERVIEW: f32 = 280.;
    if server && !hidden && window >= crate::gui::layout::RAIL_WIDTH + sidebar + OVERVIEW + crate::gui::layout::CONVERSATION_MIN + right.unwrap_or(0.) {
        OVERVIEW
    } else {
        0.
    }
}

/// "General / Tiny Client Official Server". The server's own title wins over the account host.
pub(in crate::gui) fn compact_place_label(room: &str, server_title: Option<&str>, account_name: &str) -> String {
    let server = server_title.map(str::trim).filter(|name| !name.is_empty()).unwrap_or_else(|| account_name.trim());
    if room.is_empty() {
        return server.to_owned();
    }
    if server.is_empty() {
        room.to_owned()
    } else {
        format!("{room} / {server}")
    }
}

fn question(confirm: &Confirm, here: &str) -> Question {
    let verb = if confirm.answer { "answer" } else { "call" };
    Question {
        title: format!("Leave {here} and {verb} {}?", confirm.title),
        stay: format!("Stay in {here}"),
        go: if confirm.answer { "Leave & answer" } else { "Leave & call" },
    }
}

/// Which sidebar dock layout to draw for an active call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockMode {
    /// Full dock card with mute, deafen, video, options, and leave (shown when browsing away from
    /// the call stage and no Tiny user panel is present).
    Full,
    /// Compact voice-connected card above the Tiny server user panel (which owns mute/deafen).
    CompactTiny,
    /// Compact call card while the main-pane call stage bar is visible, avoiding duplicated
    /// mute/deafen/share/camera/options buttons.
    CompactStage,
}

/// Decides how the sidebar dock should draw an active call so controls are never duplicated with
/// the main-pane stage bar or the Tiny server user panel.
pub(super) fn dock_mode(in_tiny_room: bool, tiny_column_has: bool, stage_bar_visible: bool) -> DockMode {
    if in_tiny_room && tiny_column_has {
        DockMode::CompactTiny
    } else if stage_bar_visible {
        DockMode::CompactStage
    } else {
        DockMode::Full
    }
}

/// Clamps a dock popover's width to the sidebar dock and viewport so it never bleeds across
/// the sidebar hairline or off a narrow window.
pub(in crate::gui) fn dock_popover_width(requested: f32, dock_w: f32, viewport_w: f32) -> f32 {
    let viewport_cap = (viewport_w - 16.).max(200.);
    let sidebar_cap = if dock_w > 0. { dock_w.max(200.) } else { requested };
    requested.min(sidebar_cap).min(viewport_cap)
}

/// Width and top offset of the floating incoming call overlay when the sidebar dock is hidden.
pub(in crate::gui) fn incoming_overlay_frame(viewport_w: f32, live_bar: bool) -> (f32, f32) {
    let w = (viewport_w - 32.).max(200.).min(320.);
    let top = if live_bar { 56. + theme::BANNER } else { 56. };
    (w, top)
}

impl Shell {
    /// The dock, for the end of the sidebar column as a flex-none child: full width, as tall as
    /// its rows. The narrowest sidebar still shows the server name on its own line. `None` when
    /// there is no call.
    pub(in crate::gui) fn call_dock(
        &self,
        snapshot: &Snapshot,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        ensure_keys(cx);
        ensure_daemon();
        #[cfg(feature = "gui-shot")]
        let (ended, confirm) = (super::shot::ended(), super::shot::confirm());
        #[cfg(not(feature = "gui-shot"))]
        let (ended, confirm): (Option<Ended>, Option<Confirm>) = (None, None);
        let ended = ended.as_ref().or(self.call_state.ended.as_ref());
        let confirm = confirm.as_ref().or(self.call_state.confirm.as_ref());
        let card = match self.call_shown(snapshot) {
            Some(call) => {
                let subject = Subject::of(self, snapshot, &call);
                match (&call.state, confirm) {
                    (CallState::Failed { reason, detail }, _) => {
                        self.call_failed(&call, &subject, *reason, detail, cx)
                    }
                    (CallState::RingingIn { from }, _) => {
                        self.call_incoming(&call, &subject, from, window, cx)
                    }
                    (_, Some(confirm)) => {
                        self.call_question(&question(confirm, &subject.title), cx)
                    }
                    _ => match dock_mode(
                        self.tiny_room(&call.conversation).is_some(),
                        self.tiny_column_has(&call.conversation),
                        self.call_stage_bar_visible(snapshot),
                    ) {
                        DockMode::CompactTiny => div().child(self.call_compact(&call, &subject, window, cx)),
                        DockMode::CompactStage => div().child(self.call_compact_stage(&call, &subject, window, cx)),
                        DockMode::Full => div().child(self.call_bar(&call, &subject, window, cx)),
                    },
                }
            }
            None => match self.call_state.rating.as_ref() {
                Some(rating) => self.call_rating(rating, cx),
                None => self.call_ended(ended?, cx),
            },
        };
        let dock_picture = self
            .call_shown(snapshot)
            .and_then(|call| self.call_dock_picture(snapshot, &call, cx));
        let dock_bounds = self.call_state.dock_bounds.clone();
        Some(
            div()
                .w_full()
                .flex_none()
                .px(px(8.))
                // 8 with the hairline.
                .pt(px(7.))
                .pb(px(8.))
                .border_t_1()
                .border_color(theme::hairline())
                .children(dock_picture)
                .child(
                    div()
                        .id("call-controls")
                        .relative()
                        .role(gpui::Role::Group)
                        .aria_label("Call controls")
                        .child(card)
                        .child(
                            canvas(move |bounds, _, _| dock_bounds.set(Some(bounds)), |_, _, _, _| {})
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                        ),
                ),
        )
    }

    /// The sidebar's call dock is on screen: in the row, floating over the pane, or as the
    /// whole list. A hidden or too-narrow sidebar leaves an incoming call with no dock.
    fn sidebar_beside(&self, snapshot: &Snapshot) -> bool {
        if crate::gui::phone::active() {
            return false;
        }
        let state = &self.layout_state;
        if state.floating {
            return true;
        }
        let Some((_, _, width, _)) = self.frame.get() else {
            return !state.sidebar_hidden;
        };
        let right = state.right.as_ref().filter(|_| !self.pane_tucked()).map(|_| state.right_width);
        let discord = matches!(
            &self.sidebar_state.mode,
            crate::gui::sidebar::Mode::Space(space) if space.network == Network::Discord
        );
        let overview = discord_column(width, state.sidebar_width, state.sidebar_hidden, right, discord);
        let planned = crate::gui::layout::plan(width - overview, state.sidebar_width, state.sidebar_hidden, right);
        if planned.sidebar.is_some() {
            return true;
        }
        let replaced = planned.right.is_some_and(|(_, replaces)| replaces);
        // The list, dock included, fills the window only when nothing else has the pane.
        // A call stage or a Tiny server page replaces that list, so the dock is not there.
        let list_alone = snapshot.open.is_none()
            && !state.sidebar_hidden
            && !replaced
            && !self.call_view_active(snapshot);
        if list_alone {
            let tiny_page = matches!(self.sidebar_state.mode, crate::gui::sidebar::Mode::Network(Network::Tiny)) && self.tiny_open();
            return !tiny_page;
        }
        false
    }

    /// Incoming call when the sidebar dock is not beside the pane: a card at the top right.
    pub(in crate::gui) fn call_incoming_overlay(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<AnyElement> {
        if crate::gui::phone::active() || self.sidebar_beside(snapshot) {
            return None;
        }
        let ringing = snapshot
            .ring
            .as_ref()
            .filter(|ring| matches!(ring.state, CallState::RingingIn { .. }))
            .cloned()
            .or_else(|| self.call_shown(snapshot).filter(|call| matches!(call.state, CallState::RingingIn { .. })))?;
        let subject = Subject::of(self, snapshot, &ringing);
        let CallState::RingingIn { from } = &ringing.state else {
            return None;
        };
        let name = if from.is_empty() { subject.title.clone() } else { from.clone() };
        let video = ringing.participants.iter().any(|person| person.camera)
            || ringing.video.iter().any(|stream| stream.content == Content::Camera);
        let line = if video { "Incoming video call" } else { "Incoming voice call" };
        let account = ringing.conversation.account();
        let picture = if ringing.conversation.network == Network::Tiny {
            ringing
                .participants
                .first()
                .and_then(|person| self.tiny_member_picture(&account, &person.id))
                .or_else(|| self.media_path(&ringing.conversation, subject.avatar.as_ref()))
        } else {
            self.media_path(&ringing.conversation, subject.avatar.as_ref()).or_else(|| {
                ringing.participants.first().and_then(|person| {
                    person.avatar.as_ref().and_then(|file| self.media_path(&Key::new(&account, &person.id), Some(file)))
                })
            })
        };
        let support = self
            .sidebar_state
            .find(&self.trees, &ringing.conversation)
            .map(|c| c.call)
            .unwrap_or(protocol::CallSupport::Native);
        let accept_label = incoming_accept_label(&ringing, support);
        let accept_icon = if video || ringing.incoming_video { "icons/video.svg" } else { "icons/phone.svg" };
        let conv_key = ringing.conversation.clone();
        let handoff = support == protocol::CallSupport::Handoff;
        let decline = worded("call-overlay-decline", Some("icons/phone-off.svg"), "Decline", theme::danger_soft(), theme::danger())
            .flex_1()
            .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.call_decline(cx);
            }));
        let accept = worded("call-overlay-accept", Some(accept_icon), accept_label, theme::positive(), theme::on_accent())
            .flex_1()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if handoff {
                    this.call_handoff(conv_key.clone(), cx);
                } else {
                    this.call_accept(cx);
                }
            }));
        let viewport_w = self.frame.get().map_or(1120., |(_, _, w, _)| w);
        let live_bar = !self.live_location_state.all_shares().is_empty();
        let (card_w, top) = incoming_overlay_frame(viewport_w, live_bar);
        Some(
            deferred(
                div()
                    .id("call-incoming-overlay")
                    .occlude()
                    .absolute()
                    .top(px(top))
                    .right(px(16.))
                    .w(px(card_w))
                    .max_w_full()
                    .p(px(12.))
                    .rounded(px(theme::R_MODAL))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .bg(theme::overlay())
                    .border_1()
                    .border_color(theme::overlay_edge())
                    .shadow(theme::shadow_popover())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(Avatar::new(&ringing.conversation, &name, 40.).image_at(picture).build())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .child(
                                        div()
                                            .truncate()
                                            .type_body()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::text())
                                            .child(name),
                                    )
                                    .child(div().type_small().text_color(theme::text_soft()).child(line)),
                            ),
                    )
                    .child(div().flex().gap(px(8.)).child(decline).child(accept)),
            )
            .priority(Layer::Popover as usize)
            .into_any_element(),
        )
    }

    /// The other side hung up: who it was with, how long it ran, and Dismiss. It goes by
    /// itself after a few seconds.
    fn call_ended(&self, ended: &Ended, cx: &mut Context<Self>) -> Div {
        let network = ended.conversation.network;
        let subject = &ended.subject;
        let lead = if subject.kind == ConversationKind::Voice {
            div()
                .size(px(28.))
                .flex_none()
                .rounded(px(8.))
                .bg(theme::hover())
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("icons/volume-2.svg", 14., theme::text_soft()))
        } else {
            let picture = self.media_path(&ended.conversation, subject.avatar.as_ref());
            ringed(Avatar::new(&ended.conversation, &subject.title, 28.).image_at(picture).build(), 28., false)
        };
        let dismiss = div()
            .id("call-ended-dismiss")
            .h(px(32.))
            .px(px(14.))
            .rounded(px(8.))
            .bg(theme::hover())
            .flex()
            .items_center()
            .type_ui()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .cursor_pointer()
            .hover(|style| style.bg(theme::selected()))
            .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)))
            .child("Dismiss");
        let title = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .line_clamp(2)
                            .type_ui()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(subject.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(4.))
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(network_icon(network, 11.))
                            .child(div().line_clamp(2).child(format!("Call ended · {}", clock(ended.seconds)))),
                    ),
            );
        card().child(title).child(div().min_h(px(32.)).flex().flex_wrap().items_center().child(dismiss.ml_auto()))
    }

    /// Post-call rating card: five stars, issue chips below five, optional comment, Send and Skip.
    fn call_rating(&self, rating: &super::CallRatingCard, cx: &mut Context<Self>) -> Div {
        let subtitle = match rating.prompt.seconds {
            Some(secs) => format!("{} · {}", rating.title, clock(i64::from(secs))),
            None => rating.title.clone(),
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .type_ui()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Rate call quality"),
            )
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(subtitle),
            );
        let mut stars_row = div().flex().items_center().gap(px(4.));
        for n in 1..=5u8 {
            let filled = n <= rating.stars;
            stars_row = stars_row.child(
                div()
                    .id(gpui::SharedString::from(format!("call-rate-star-{n}")))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Rate {n} of 5 stars"))
                    .size(px(26.))
                    .rounded(px(6.))
                    .bg(if filled { theme::accent_soft() } else { theme::hover() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tab_index(0)
                    .focus_visible(|s| s.shadow(theme::focus_ring()))
                    .hover(|s| s.bg(theme::selected()))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_rating_star(n, cx)))
                    .child(glyph("icons/star.svg", 14., if filled { theme::accent() } else { theme::text_faint() })),
            );
        }
        let reasons = (rating.stars > 0 && rating.stars < 5).then(|| {
            let mut wrap = div().flex().flex_wrap().gap(px(4.));
            for &reason in super::CALL_RATING_REASONS {
                let picked = rating.reasons.contains(&reason);
                wrap = wrap.child(
                    div()
                        .id(gpui::SharedString::from(format!("call-rate-reason-{reason}")))
                        .px(px(7.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .bg(if picked { theme::accent_soft() } else { theme::hover() })
                        .type_micro()
                        .text_color(if picked { theme::accent_text() } else { theme::text_soft() })
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::selected()))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_rating_toggle_reason(reason, cx)))
                        .child(reason),
                );
            }
            wrap
        });
        let comment = (rating.stars > 0).then(|| {
            div()
                .w_full()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::hairline())
                .type_small()
                .text_color(theme::text())
                .child(rating.comment_input.clone())
        });
        let error = rating.error.as_ref().map(|msg| {
            div()
                .type_micro()
                .text_color(theme::danger())
                .child(msg.clone())
        });
        let can_send = rating.stars > 0 && !rating.sending;
        let skip = div()
            .id("call-rate-skip")
            .h(px(28.))
            .px(px(10.))
            .rounded(px(7.))
            .bg(theme::hover())
            .flex()
            .items_center()
            .type_small()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .cursor_pointer()
            .hover(|s| s.bg(theme::selected()))
            .on_click(cx.listener(|this, _, _, cx| this.call_rating_skip(cx)))
            .child("Skip");
        let send = div()
            .id("call-rate-send")
            .h(px(28.))
            .px(px(12.))
            .rounded(px(7.))
            .bg(if can_send { theme::accent() } else { theme::hover() })
            .flex()
            .items_center()
            .type_small()
            .font_weight(FontWeight::MEDIUM)
            .text_color(if can_send { theme::on_accent() } else { theme::text_faint() })
            .when(can_send, |d| d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| this.call_rating_send(cx))))
            .child(if rating.sending { "Sending…" } else { "Send" });
        card()
            .child(header)
            .child(stars_row)
            .children(reasons)
            .children(comment)
            .children(error)
            .child(div().flex().items_center().justify_end().gap(px(6.)).child(skip).child(send))
    }

    /// "Leave Lounge and call Mara Okafor?": asked in the card, where the call is, before a
    /// call is left for another.
    fn call_question(&self, question: &Question, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, word: String| {
            div()
                .id(id)
                .flex_1()
                .min_w(px(120.))
                .min_h(px(32.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .type_ui()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(div().line_clamp(2).child(word))
        };
        let stay = button("call-stay", question.stay.clone())
            .bg(theme::hover())
            .text_color(theme::text())
            .hover(|style| style.bg(theme::selected()))
            .on_click(cx.listener(|this, _, _, cx| this.call_stay(cx)));
        let go = button("call-go", question.go.to_owned())
            .bg(theme::positive())
            .text_color(theme::on_accent())
            .on_click(cx.listener(|this, _, _, cx| this.call_confirmed(cx)));
        card()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .line_clamp(3)
                            .type_ui()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(question.title.clone()),
                    )
                    .child(
                        div()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child("You can be in one call at a time."),
                    ),
            )
            .child(div().flex().flex_wrap().gap(px(4.)).child(stay).child(go))
    }

    /// A ring waiting beside the call that is up. `--shot` can still name one of its own.
    /// When the call slot is empty, that ring is the card itself, not a strip on top of it.
    fn call_waiting_strip(&self) -> Option<(Confirm, String)> {
        #[cfg(feature = "gui-shot")]
        if let Some(strip) = super::shot::strip() {
            return Some(strip);
        }
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.call.is_none() {
            return None;
        }
        let ring = snapshot.ring.as_ref()?;
        let CallState::RingingIn { from } = &ring.state else {
            return None;
        };
        let confirm = Confirm {
            conversation: ring.conversation.clone(),
            title: from.clone(),
            answer: true,
        };
        Some((confirm, from.clone()))
    }

    /// Someone is calling while a call is up: a 40 px strip on top of the bar, with who and
    /// where, Decline and Accept. Accept asks before it leaves the call that is up.
    fn call_strip(&self, ring: &Confirm, from: &str, cx: &mut Context<Self>) -> Div {
        let mini = |id: &'static str, icon: &'static str, fill: Rgba, color: Rgba| {
            div()
                .id(id)
                .flex_none()
                .w(px(32.))
                .h(px(26.))
                .rounded(px(7.))
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(glyph(icon, 14., color))
        };
        let accept = ring.clone();
        div()
            .min_h(px(40.))
            .flex_none()
            // Out to the card's edges, over its padding.
            .mt(px(-9.))
            .mx(px(-9.))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme::hairline())
            .type_small()
            .text_color(theme::text_soft())
            .child(Avatar::named(from, 20.).build())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .whitespace_normal()
                            .line_clamp(2)
                            .child(from.to_owned()),
                    )
                    .child(div().whitespace_normal().line_clamp(2).child(if from == ring.title {
                        "is calling".to_owned()
                    } else {
                        format!("is calling · {}", ring.title)
                    })),
            )
            .child(
                mini("call-strip-decline", "icons/phone-off.svg", theme::danger_soft(), theme::danger())
                    .tooltip(tip("Decline", Some("⌘⇧R")))
                    .on_click(cx.listener(|this, _, _, cx| this.call_ask(protocol::Request::CallDecline, cx))),
            )
            .child(
                mini("call-strip-accept", "icons/phone.svg", theme::positive(), theme::on_accent())
                    .tooltip(tip("Accept", Some("⌘⇧A")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.call_state.confirm = Some(accept.clone());
                        cx.notify();
                    })),
            )
    }

    /// Someone in this call started sharing their screen: a 32 px strip on top of the card
    /// naming who is sharing, with one-click Watch and a dismiss button.
    fn call_share_strip(
        &self,
        call: &Call,
        subject: &Subject,
        first_strip: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if call.state != CallState::Joined {
            return None;
        }
        let sharer_id = self.call_state.share_prompt.as_deref()?;
        if super::watching(call, sharer_id) {
            return None;
        }
        let sharer = call.participants.iter().find(|p| p.id == sharer_id && !p.ringing && p.sharing)?;
        let watch_call = call.clone();
        let watch_id = sharer.id.clone();
        let watch_title = subject.title.clone();
        Some(
            div()
                .min_h(px(32.))
                .flex_none()
                .when(first_strip, |strip| strip.mt(px(-9.)).rounded_t(px(11.)))
                .mx(px(-9.))
                .pl(px(10.))
                .pr(px(8.))
                .py(px(5.))
                .bg(theme::accent_soft())
                .border_b_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(theme::text())
                .child(glyph("icons/monitor-up.svg", 13., theme::accent()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{} is sharing their screen", sharer.name)),
                )
                .child(
                    div()
                        .id("call-share-strip-watch")
                        .flex_none()
                        .h(px(22.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(theme::accent())
                        .hover(|s| s.bg(theme::accent_hover()))
                        .text_color(theme::on_accent())
                        .type_small()
                        .font_weight(FontWeight::MEDIUM)
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_watch_stream(&watch_call, watch_id.clone(), &watch_title, cx);
                        }))
                        .child("Watch"),
                )
                .child(
                    div()
                        .id("call-share-strip-dismiss")
                        .flex_none()
                        .size(px(20.))
                        .rounded(px(5.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(tip("Dismiss", None))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_dismiss_share_prompt(cx);
                        }))
                        .child(glyph("icons/x.svg", 12., theme::text_soft())),
                ),
        )
    }

    /// The counter's text, told what it counts. Nothing ticks unless this is on screen.
    fn call_counter(&self, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.call_dock_clock(call, cx)
    }

    /// The signal bars of a call whose connection is measured (a Tiny call), which open Call
    /// health. Nothing for a call that is not up, or whose numbers are not read.
    fn call_signal(&self, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        if call.state != CallState::Joined {
            return None;
        }
        let health = self.tiny_health().filter(|_| call.conversation.network == Network::Tiny)?;
        let grade = health.grade();
        let words = match (grade, health.rtt_ms) {
            (Grade::Unknown, _) | (_, None) => grade.words().to_owned(),
            (_, Some(ms)) => format!("{} · {ms} ms", grade.words()),
        };
        Some(
            div()
                .id("call-signal")
                .flex_none()
                .size(px(24.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.bg(theme::hover()))
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .tooltip(tip(format!("{words}. Call health"), None))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.tiny_toggle_health(cx);
                }))
                .child(health::bars(grade, 12.))
                .into_any_element(),
        )
    }

    /// Share and camera belong on a joined call that allows video (`Call::video_allowed`) on a
    /// network that carries it (Tiny, Discord, Signal, or Telegram — excluding RTMP viewer streams).
    fn call_pictures(&self, call: &Call) -> bool {
        call.state == CallState::Joined
            && call.video_allowed
            && !call.rtmp_stream
            && protocol::call_pictures(call.conversation.network)
    }

    /// Where the compact bar says the call is. A Tiny server's title replaces the raw host.
    fn compact_place(&self, call: &Call, subject: &Subject) -> String {
        if call.conversation.network != Network::Tiny {
            return match &subject.space {
                Some(server) => format!("{} / {server}", subject.title),
                None => subject.title.clone(),
            };
        }
        let account = call.conversation.account();
        // Copy the title out before reading the account name. The page borrows `self`.
        let server_title = (self.tiny_state.account.as_ref() == Some(&account))
            .then(|| self.tiny_page_facts())
            .flatten()
            .and_then(|page| page.community.server.as_ref().map(|server| server.name.trim().to_owned()))
            .filter(|name| !name.is_empty());
        let account_name = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.account(&account))
            .map(|status| status.name.clone())
            .unwrap_or_default();
        compact_place_label(&subject.title, server_title.as_deref(), &account_name)
    }

    /// Over a Tiny server's column: "Voice connected", where, how well, and Disconnect. Mute,
    /// deafen and the voice settings are in the user panel right under it.
    fn call_compact(&self, call: &Call, subject: &Subject, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let (state, color) = match call.state {
            CallState::Joined if super::securing(call) => ("Securing…", theme::warning()),
            CallState::Joined if self.call_muted_talking(call) => ("You're muted", theme::danger()),
            // Coloured by the link, as the bars are: green, amber when fair, red when poor.
            CallState::Joined => match self.tiny_health().map(|health| health.grade()) {
                Some(grade @ (Grade::Fair | Grade::Poor)) => ("Voice connected", grade.color()),
                _ => ("Voice connected", theme::positive()),
            },
            CallState::Reconnecting => ("Reconnecting…", theme::warning()),
            _ => ("Connecting…", theme::warning()),
        };
        let notice = self.call_state.error.clone().or_else(|| self.call_state.notice.clone().filter(|_| in_call(call)));
        let clock = if notice.is_some() && call.state != CallState::Joined { None } else { self.call_counter(call, cx) };
        let place = self.compact_place(call, subject);
        let key = call.conversation.clone();
        let title = subject.title.clone();
        let words = div()
            .id("call-compact-where")
            .flex_1()
            .min_w_0()
            .px(px(4.))
            .py(px(2.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .type_ui()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .when(self.call_muted_talking(call), |row| row.child(glyph("icons/mic-off.svg", 13., theme::danger())))
                    .child(div().min_w_0().truncate().child(state))
                    .children(clock.map(|clock| div().flex_none().font_weight(FontWeight::NORMAL).text_color(theme::text_soft()).child(clock))),
            )
            .child(
                div()
                    .whitespace_normal()
                    .line_clamp(4)
                    .type_small()
                    .text_color(if notice.is_some() { theme::warning() } else { theme::text_soft() })
                    .child(notice.unwrap_or(place)),
            );
        let people_chip = in_call(call).then(|| {
            let count = super::call_people_count(call);
            let open = self.call_menu() == Some(Menu::People);
            let fg = if open { theme::text() } else { theme::text_soft() };
            div()
                .id("call-compact-people")
                .flex_none()
                .h(px(24.))
                .px(px(6.))
                .rounded(px(theme::R_CONTROL))
                .bg(if open { theme::selected() } else { theme::hover() })
                .hover(|style| style.bg(theme::selected()).text_color(theme::text()))
                .flex()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .type_small()
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .tooltip(tip("People in this call", None))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::People, cx);
                }))
                .child(glyph("icons/users.svg", 12., fg))
                .child(count.to_string())
        });
        let disconnect = leave()
            .w(px(36.))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.call_leave(cx);
            }));
        let waiting = self.call_waiting_strip();
        let share_strip = self.call_share_strip(call, subject, waiting.is_none(), cx);
        // The name takes the card's width. Signal, lock, people and Leave sit on the row above it,
        // and wrap under it when the column is the narrowest sidebar.
        card()
            .id("call-compact-card")
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .hover(|style| style.border_color(theme::border()))
            .tooltip(tip("Open the call", Some("⌘⇧J")))
            .on_click(cx.listener(move |this, _, _, cx| this.call_go(key.clone(), &title, cx)))
            .children(waiting.map(|(ring, from)| self.call_strip(&ring, &from, cx)))
            .children(share_strip)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .children(self.call_signal(call, cx))
                    .children(self.call_security_in(call, Place::TinyBar, Some(window), cx))
                    .children(people_chip)
                    .child(disconnect.ml_auto()),
            )
            .child(words)
            .children(self.call_popover(call, subject, Place::TinyBar, window, cx))
    }

    /// Compact sidebar dock card shown while the main-pane call stage is open: keeps the call's
    /// identity, lock/signal/duration, any dock-only actions with no twin on the stage bar
    /// (Ring again, Copy call link, Telegram video-chat options), and Leave without duplicating
    /// mute, deafen, share, camera, call options, or the stage header's Show chat button.
    fn call_compact_stage(&self, call: &Call, subject: &Subject, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let place = subject
            .space
            .as_deref()
            .unwrap_or(call.conversation.network.label());
        let alone = self.call_left_alone(call);
        let ringing = call.ringing.iter().filter(|ring| ring.state == "ringing").count();
        let words = match (&call.state, alone.as_deref()) {
            (CallState::Connecting | CallState::Reconnecting, _) if call.moved_from.is_some() => {
                "Moving the call…".to_owned()
            }
            (CallState::RingingOut, _) => "Ringing…".to_owned(),
            (CallState::Connecting, _) => "Connecting…".to_owned(),
            (CallState::Reconnecting, _) => "Reconnecting…".to_owned(),
            (_, Some(peer)) if subject.kind == ConversationKind::Direct => {
                format!("{} left", peer.split_whitespace().next().unwrap_or(peer))
            }
            (CallState::Joined, _)
                if subject.kind == ConversationKind::Group && call.participants.is_empty() && ringing > 0 =>
            {
                format!("Ringing {ringing}…")
            }
            (CallState::Joined, _) if subject.kind == ConversationKind::Group && call.participants.is_empty() => {
                "Waiting for others".to_owned()
            }
            _ => place.to_owned(),
        };
        let joined = call.state == CallState::Joined;
        let enabled = call.state != CallState::Reconnecting;
        let menu = self.call_menu().filter(|_| enabled);
        let ring_again = self
            .call_ring_again_button(call, subject, joined, enabled, alone.is_some(), cx)
            .map(|btn| btn.w(px(34.)).h(px(30.)));
        let copy_link = self
            .call_copy_link_button(call, joined, enabled, cx)
            .map(|btn| btn.w(px(34.)).h(px(30.)));
        let tg_actions = self
            .call_tg_actions_button(call, subject, joined, enabled, menu.as_ref(), cx)
            .map(|btn| btn.w(px(34.)).h(px(30.)));
        let in_live_call = in_call(call);
        let end = if in_live_call {
            leave().w(px(40.)).h(px(30.))
        } else {
            worded(
                "call-cancel",
                None,
                "Cancel",
                theme::danger_soft(),
                theme::danger(),
            )
            .h(px(30.))
            .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
            .tooltip(tip("Cancel", Some("⌘⇧H")))
        }
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            if in_live_call {
                this.telegram_leave_or_end(cx);
            } else {
                this.call_leave(cx);
            }
        }));
        let waiting = self.call_waiting_strip();
        let share_strip = self.call_share_strip(call, subject, waiting.is_none(), cx);
        card()
            .id("call-compact-stage-card")
            .children(waiting.map(|(ring, from)| self.call_strip(&ring, &from, cx)))
            .children(share_strip)
            .child(self.call_title(call, subject, words, window, cx))
            .child(
                div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(ring_again)
                    .children(copy_link)
                    .children(tg_actions)
                    .child(end.ml_auto()),
            )
            .children(self.call_popover(call, subject, Place::Dock, window, cx))
    }

    fn call_ring_again_button(
        &self,
        call: &Call,
        subject: &Subject,
        joined: bool,
        enabled: bool,
        alone: bool,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        (joined
            && enabled
            && ((matches!(call.conversation.network, Network::Discord | Network::Signal)
                && subject.kind.is_direct()
                && alone)
                || (call.conversation.network == Network::Signal && subject.kind == ConversationKind::Group)))
        .then(|| {
            control("call-ring-again", "icons/phone-outgoing.svg", Look::Rest, true)
                .aria_label("Ring again")
                .tooltip(tip("Ring again", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_ask(Request::CallRing { people: None }, cx);
                }))
        })
    }

    fn call_copy_link_button(
        &self,
        call: &Call,
        joined: bool,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        call.invite_link
            .clone()
            .filter(|link| !link.is_empty() && joined && enabled)
            .map(|link| {
                control("call-copy-link", "icons/link.svg", Look::Rest, true)
                    .aria_label("Copy call link")
                    .tooltip(tip("Copy call link", None))
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                    }))
            })
    }

    fn call_tg_actions_button(
        &self,
        call: &Call,
        subject: &Subject,
        joined: bool,
        enabled: bool,
        menu: Option<&Menu>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        (call.conversation.network == Network::Telegram
            && matches!(subject.kind, ConversationKind::Group | ConversationKind::Channel)
            && joined
            && enabled)
            .then(|| {
                control(
                    "call-tg-actions",
                    "icons/ellipsis.svg",
                    if menu == Some(&Menu::TelegramCall) { Look::Open } else { Look::Rest },
                    true,
                )
                .aria_label("Video chat options")
                .tooltip(tip("Video chat options", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::TelegramCall, cx);
                }))
            })
    }

    /// The popover hung above the call dock card or the compact voice bar.
    fn call_popover(
        &self,
        call: &Call,
        subject: &Subject,
        place: Place,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let joined = call.state == CallState::Joined;
        let enabled = call.state != CallState::Reconnecting;
        let menu = self.call_menu().filter(|_| enabled)?;
        let stage_visible = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| self.call_stage_bar_visible(snapshot));
        let (width, body) = match (place, &menu) {
            (Place::Dock | Place::TinyBar, _)
                if !stage_visible
                    && matches!(
                        menu,
                        Menu::Devices
                            | Menu::Input { .. }
                            | Menu::Output { .. }
                            | Menu::Camera { .. }
                            | Menu::CameraPreview
                    ) =>
            {
                self.call_device_popover_body(call, &menu, cx)?
            }
            (_, Menu::People) if joined => (280., self.call_people_list(call, subject, cx).py(px(6.))),
            (Place::Dock, Menu::AddPeople) if joined && !stage_visible => {
                (280., self.call_add_people_menu(call, cx).p(px(6.)))
            }
            (Place::Dock, Menu::Security) if self.call_security_open(Place::Dock) => {
                (320., div().child(self.call_security_panel(call, cx)))
            }
            (Place::Dock, Menu::Soundboard)
                if joined && !stage_visible && call.conversation.network == Network::Discord =>
            {
                (320., self.call_soundboard_body(call, cx).p(px(8.)))
            }
            (_, Menu::Person { account, user_id, name }) if joined && !stage_visible => {
                (260., self.call_person_menu(account.clone(), user_id, name, cx).p(px(6.)))
            }
            (_, Menu::TelegramCall) if joined => {
                (280., self.telegram_call_menu(call, cx).p(px(6.)))
            }
            _ => return None,
        };
        let viewport = window.viewport_size();
        let wide = f32::from(viewport.width);
        let tall = f32::from(viewport.height);
        let captured = self.call_state.dock_bounds.get();
        let live_sidebar = self.sidebar_state.width.get();
        let sidebar = if live_sidebar > 0. { live_sidebar } else { self.layout_state.sidebar_width };
        let dock_w = captured
            .map(|b| f32::from(b.size.width))
            .filter(|w| *w > 0.)
            .unwrap_or_else(|| (sidebar - 16.).max(200.));
        let width = dock_popover_width(width, dock_w, wide);
        let fallback = anchor(
            crate::gui::layout::RAIL_WIDTH + 8.,
            (tall - 96.).max(40.),
            dock_w.max(200.),
            80.,
        );
        let bounds = captured.unwrap_or(fallback);
        let panel = div()
            .id("call-popover")
            .w(px(width))
            .max_w_full()
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::border())
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.call_menu_close(cx)))
            .child(body);
        Some(placed(
            "call-popover-layer",
            OverlayPlace::Above(bounds),
            Layer::Popover,
            window,
            panel,
        ))
    }

    /// Who or where, then the server or notice on its own line, then signal, lock and who else
    /// is there. Those icons used to share the name's row, which is what cut a server down to
    /// "Tiny C…" at the narrowest sidebar. A click opens the call's conversation.
    fn call_title(&self, call: &Call, subject: &Subject, words: String, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let network = call.conversation.network;
        let direct = subject.kind == ConversationKind::Direct;
        let live = in_call(call);
        let lead = if subject.kind == ConversationKind::Voice {
            div()
                .size(px(28.))
                .flex_none()
                .rounded(px(8.))
                .bg(theme::positive_soft())
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("icons/volume-2.svg", 14., theme::positive()))
        } else {
            let peer = call.participants.first();
            let peer_speaks = live && direct && peer.is_some_and(|peer| peer.speaking);
            let picture = self.media_path(&call.conversation, subject.avatar.as_ref());
            let avatar = Avatar::new(&call.conversation, &subject.title, 28.).image_at(picture);
            let ring = ringed(avatar.build(), 28., peer_speaks);
            if let Some(peer) = peer.filter(|_| live && direct) {
                let account = call.conversation.account();
                let user_id = peer.id.clone();
                let name = peer.name.clone();
                ring.on_mouse_down(
                    gpui::MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_menu_set(Some(Menu::Person { account: account.clone(), user_id: user_id.clone(), name: name.clone() }), cx);
                    }),
                )
            } else {
                ring
            }
        };
        // What was refused, then what the system just did, then the state.
        let notice = self.call_state.notice.clone().filter(|_| live);
        let refused = self.call_state.error.clone();
        let reconnecting = call.state == CallState::Reconnecting;
        let talking = self.call_muted_talking(call);
        let color = if talking {
            theme::danger()
        } else if reconnecting || refused.is_some() || (live && call.device_stuck) {
            theme::warning()
        } else {
            theme::text_soft()
        };
        let peer_note = live.then(|| {
            let paused = call.participants.iter().any(|p| p.camera_paused);
            let low_bat = call.participants.iter().any(|p| p.low_battery);
            match (paused, low_bat) {
                (true, true) => Some("Camera paused · Low battery".to_owned()),
                (true, false) => Some("Camera paused".to_owned()),
                (false, true) => Some("Peer battery low".to_owned()),
                (false, false) => None,
            }
        }).flatten();
        let words = if talking {
            format!("You're muted. Press {} to talk", crate::gui::shortcuts::label("⌘⇧M"))
        } else if let Some(note) = peer_note {
            note
        } else {
            words
        };
        let counter = if refused.is_some() || notice.is_some() { None } else { self.call_counter(call, cx) };
        let refused = refused.or(notice);
        // Give the server name or notice the full line; status and clock have their own row.
        let place = div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .type_small()
            .text_color(color)
            .child(div().flex_none().child(network_icon(network, 11.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(refused.unwrap_or(words)),
            )
            .children(self.call_switch_device(call, live, cx));
        let right: Option<AnyElement> = match (live, direct) {
            (false, _) => None,
            (true, true) => call
                .participants
                .first()
                .filter(|peer| peer.muted)
                .map(|_| glyph("icons/mic-off.svg", 13., theme::text_faint()).into_any_element()),
            (true, false) if call.participants.is_empty() => None,
            (true, false) => {
                let account = call.conversation.account();
                Some(
                    people::stack(call, |who| self.call_participant_picture(&account, who, cx))
                        .id("call-stack")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_menu_toggle(Menu::People, cx);
                        }))
                        .into_any_element(),
                )
            }
        };
        let signal = self.call_signal(call, cx);
        let security = self.call_security_in(call, Place::Dock, Some(window), cx);
        let show_status = signal.is_some() || security.is_some() || right.is_some() || counter.is_some();
        div()
            .id("call-title")
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(lead)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .type_ui()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(super::telegram::call_header_display_title(call, &subject.title)),
                    )
                    .children(
                        call.record_start_date
                            .map(|since| super::telegram::recording_badge("call-dock-rec", since, call.record_video_active)),
                    ),
            )
            .child(place)
            .when(show_status, |column| {
                column.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(2.))
                        .children(signal)
                        .children(security)
                        .children(right)
                        .children(counter),
                )
            })
    }

    /// Ringing out, connecting, joined and reconnecting. As tall as the name, the server line
    /// and the controls, which wrap when the sidebar is at its narrowest.
    fn call_bar(&self, call: &Call, subject: &Subject, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let place = subject
            .space
            .as_deref()
            .unwrap_or(call.conversation.network.label());
        let alone = self.call_left_alone(call);
        let ringing = call.ringing.iter().filter(|ring| ring.state == "ringing").count();
        let words = match (&call.state, alone.as_deref()) {
            (CallState::Connecting | CallState::Reconnecting, _) if call.moved_from.is_some() => "Moving the call…".to_owned(),
            (CallState::RingingOut, _) => "Ringing…".to_owned(),
            (CallState::Connecting, _) => "Connecting…".to_owned(),
            (CallState::Reconnecting, _) => "Reconnecting…".to_owned(),
            // Alone in a direct call: said, and left to me. tinyclient does not hang up for me.
            (_, Some(peer)) if subject.kind == ConversationKind::Direct => {
                format!("{} left", peer.split_whitespace().next().unwrap_or(peer))
            }
            (CallState::Joined, _)
                if subject.kind == ConversationKind::Group && call.participants.is_empty() && ringing > 0 =>
            {
                format!("Ringing {ringing}…")
            }
            (CallState::Joined, _) if subject.kind == ConversationKind::Group && call.participants.is_empty() => {
                "Waiting for others".to_owned()
            }
            _ if call.move_notice.as_ref().is_some_and(|words| !words.is_empty()) => {
                call.move_notice.clone().unwrap_or_default()
            }
            _ => place.to_owned(),
        };
        let joined = call.state == CallState::Joined;
        // Reconnecting keeps the joined controls, dead; only Leave is live.
        let enabled = call.state != CallState::Reconnecting;
        let menu = self.call_menu().filter(|_| enabled);
        let mic_mode = super::telegram::telegram_mic_button_mode(call);
        let forced_mute = call.server_muted || call.suppressed;
        let off = call.muted || call.deafened || forced_mute;
        let mute_enabled = match mic_mode {
            super::telegram::TelegramMicMode::Normal => enabled && !forced_mute,
            super::telegram::TelegramMicMode::RaiseHand | super::telegram::TelegramMicMode::LowerHand => enabled && joined,
        };
        let (mute_icon, mute_look, mute_tip) = match mic_mode {
            super::telegram::TelegramMicMode::RaiseHand => ("icons/hand.svg", Look::Rest, "Raise hand"),
            super::telegram::TelegramMicMode::LowerHand => ("icons/hand.svg", Look::On, "Lower hand"),
            super::telegram::TelegramMicMode::Normal => {
                let tip_text = if call.server_muted {
                    "Server muted"
                } else if call.suppressed {
                    "Can't speak in this channel"
                } else if off {
                    "Unmute"
                } else {
                    "Mute"
                };
                (
                    if off { "icons/mic-off.svg" } else { "icons/mic.svg" },
                    if off { Look::Off } else { Look::Rest },
                    tip_text,
                )
            }
        };

        let input_open = matches!(menu, Some(Menu::Input { .. }));
        let mute_look = if input_open && mute_look == Look::Rest {
            Look::Open
        } else {
            mute_look
        };
        let mute_tip_hint: gpui::SharedString = if mic_mode == super::telegram::TelegramMicMode::Normal && enabled {
            format!("{mute_tip} · Right-click for mic").into()
        } else {
            mute_tip.into()
        };
        let mute = control(
            "call-mute",
            mute_icon,
            mute_look,
            mute_enabled,
        )
        .aria_label(mute_tip)
        .tooltip(tip(
            mute_tip_hint,
            (mute_enabled && mic_mode == super::telegram::TelegramMicMode::Normal).then_some("⌘⇧M"),
        ))
        .when(enabled, |mute| {
            mute.on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Input { from_hub: false }, cx);
                }),
            )
        })
        .when(mute_enabled, |mute| {
            mute.on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if mic_mode == super::telegram::TelegramMicMode::Normal {
                    this.call_mute(cx);
                } else {
                    this.telegram_toggle_hand(cx);
                }
            }))
        })
        // While I am heard: a 2 px ring inside the button, the same green as a speaker's.
        .when(call.speaking && !off && joined, |mute| {
            mute.relative().child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(8.))
                    .border_2()
                    .border_color(theme::positive()),
            )
        });
        let deafen_off = call.deafened || call.server_deafened;
        let deafen_enabled = enabled && !call.server_deafened;
        let deafen_tip = if call.server_deafened {
            "Server deafened"
        } else if call.deafened {
            "Undeafen"
        } else {
            "Deafen"
        };
        let output_open = matches!(menu, Some(Menu::Output { .. }));
        let deafen_look = if deafen_off {
            Look::Off
        } else if output_open {
            Look::Open
        } else {
            Look::Rest
        };
        let deafen_tip_hint: gpui::SharedString = if enabled {
            format!("{deafen_tip} · Right-click for speaker").into()
        } else {
            deafen_tip.into()
        };
        let deafen = control(
            "call-deafen",
            if deafen_off {
                "icons/headphone-off.svg"
            } else {
                "icons/headphones.svg"
            },
            deafen_look,
            deafen_enabled,
        )
        .aria_label(deafen_tip)
        .tooltip(tip(deafen_tip_hint, deafen_enabled.then_some("⌘⇧D")))
        .when(enabled, |deafen| {
            deafen.on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Output { from_hub: false }, cx);
                }),
            )
        })
        .when(deafen_enabled, |deafen| {
            deafen.on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.call_deafen(cx);
            }))
        });
        let pictures = self.call_pictures(call);
        let share_on = encoding(call, Content::Share);
        let camera_on = encoding(call, Content::Camera);
        let share_limit = call.video_limit_reason(share_on);
        let share_enabled = enabled && share_limit.is_none();
        let share_tip: gpui::SharedString = match &share_limit {
            Some(reason) => reason.clone().into(),
            None if share_on => "Stop sharing".into(),
            None => "Share your screen".into(),
        };
        let share = pictures.then(|| {
            control(
                "call-share",
                "icons/monitor-up.svg",
                if share_on { Look::On } else { Look::Rest },
                share_enabled,
            )
            .aria_label(share_tip.clone())
            .tooltip(tip(share_tip, None))
            .when(share_enabled, |share| {
                share.on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if share_on {
                        this.tiny_stop_share(cx);
                    } else {
                        this.tiny_share(window, cx);
                    }
                }))
            })
        });
        let camera_limit = call.video_limit_reason(camera_on);
        let camera_enabled = enabled && camera_limit.is_none();
        let camera_tip: gpui::SharedString = match &camera_limit {
            Some(reason) => reason.clone().into(),
            None if camera_on => "Turn camera off · Right-click to switch".into(),
            None => "Turn camera on · Right-click to pick".into(),
        };
        let camera = pictures.then(|| {
            let call_cam = call.clone();
            let call_cam_right = call.clone();
            control(
                "call-camera",
                "icons/video.svg",
                if camera_on {
                    Look::On
                } else if matches!(menu, Some(Menu::Camera { .. } | Menu::CameraPreview)) {
                    Look::Open
                } else {
                    Look::Rest
                },
                camera_enabled,
            )
            .aria_label(if camera_on { "Turn camera off" } else { "Turn camera on" })
            .tooltip(tip(camera_tip, None))
            .when(camera_enabled, |camera| {
                camera
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_camera_picker_toggle(&call_cam_right, camera_on, cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_camera_clicked(&call_cam, camera_on, cx);
                    }))
            })
        });
        let ring_again = self.call_ring_again_button(call, subject, joined, enabled, alone.is_some(), cx);
        let hand_raised = call.hand_raised;
        let raise_hand = (call.conversation.network == Network::Signal
            && subject.kind == ConversationKind::Group
            && joined
            && enabled)
        .then(|| {
            control(
                "call-raise-hand",
                "icons/hand.svg",
                if hand_raised { Look::On } else { Look::Rest },
                true,
            )
            .aria_label(if hand_raised { "Lower hand" } else { "Raise hand" })
            .tooltip(tip(if hand_raised { "Lower hand" } else { "Raise hand" }, None))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.call_ask(Request::CallRaiseHand { raised: !hand_raised }, cx);
            }))
        });
        let picker = self.call_add_picker(call, subject.kind);
        let discord_add = !picker
            && call.conversation.network == Network::Discord
            && subject.kind.is_direct()
            && joined
            && enabled
            && (call.participants.len() + 1) < 10;
        let telegram_add = !picker
            && call.conversation.network == Network::Telegram
            && (subject.kind.is_direct() || call.conversation.id.starts_with("conf:"))
            && joined
            && enabled;
        let add_people = (picker || discord_add || telegram_add).then(|| {
            control(
                "call-add-people",
                "icons/user-plus.svg",
                if menu == Some(Menu::AddPeople) { Look::Open } else { Look::Rest },
                true,
            )
            .aria_label("Add people")
            .tooltip(tip("Add people", None))
            .on_click(cx.listener({
                let picker = picker;
                move |this, _, window, cx| {
                    cx.stop_propagation();
                    if picker {
                        this.open_call_people(window, cx);
                    } else {
                        this.call_menu_toggle(Menu::AddPeople, cx);
                    }
                }
            }))
        });
        let copy_link = self.call_copy_link_button(call, joined, enabled, cx);
        let soundboard = (in_call(call) && call.conversation.network == Network::Discord).then(|| {
            let sound_key = call.conversation.clone();
            control(
                "call-soundboard",
                "icons/audio-lines.svg",
                if menu == Some(Menu::Soundboard) { Look::Open } else { Look::Rest },
                enabled,
            )
            .aria_label("Soundboard")
            .when(enabled, |button| {
                button
                    .tooltip(tip("Soundboard", None))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_soundboard_toggle(sound_key.clone(), cx);
                    }))
            })
        });
        let devices = control(
            "call-devices",
            "icons/settings.svg",
            if menu == Some(Menu::Devices) {
                Look::Open
            } else {
                Look::Rest
            },
            enabled,
        )
        .aria_label("Call options")
        .when(enabled, |devices| {
            devices
                .tooltip(tip("Call options", None))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Devices, cx);
                }))
        });
        let tg_actions = self.call_tg_actions_button(call, subject, joined, enabled, menu.as_ref(), cx);
        let end = if in_call(call) {
            leave()
        } else {
            worded(
                "call-cancel",
                None,
                "Cancel",
                theme::danger_soft(),
                theme::danger(),
            )
            .hover(|style| {
                style
                    .bg(theme::danger_fill())
                    .text_color(theme::on_accent())
            })
            .tooltip(tip("Cancel", Some("⌘⇧H")))
        };
        let in_live_call = in_call(call);
        // Joined controls share the width equally, including when video adds two buttons.
        let controls = div()
            .w_full()
            .min_h(px(32.))
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .child(mute.when(in_live_call, |button| button.flex_1().min_w(px(36.))))
            // Deafen is for a call that is up; before that only mute and devices can be preset.
            .when(in_live_call, |row| row.child(deafen.flex_1().min_w(px(36.))))
            .children(share.map(|button| button.flex_1().min_w(px(36.))))
            .children(camera.map(|button| button.flex_1().min_w(px(36.))))
            .children(raise_hand.map(|button| button.flex_1().min_w(px(36.))))
            .children(ring_again.map(|button| button.flex_1().min_w(px(36.))))
            .children(add_people.map(|button| button.flex_1().min_w(px(36.))))
            .children(tg_actions.map(|button| button.flex_1().min_w(px(36.))))
            .children(copy_link.map(|button| button.flex_1().min_w(px(36.))))
            .children(soundboard.map(|button| button.flex_1().min_w(px(36.))))
            .child(devices.when(in_live_call, |button| button.flex_1().min_w(px(36.))))
            .child(
                end.when(in_live_call, |button| button.flex_1().min_w(px(36.)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if in_live_call {
                            this.telegram_leave_or_end(cx);
                        } else {
                            this.call_leave(cx);
                        }
                    })),
            );

        let waiting = self.call_waiting_strip();
        let share_strip = self.call_share_strip(call, subject, waiting.is_none(), cx);
        let card_key = call.conversation.clone();
        let card_title = subject.title.clone();
        card()
            .id("call-bar-card")
            .cursor_pointer()
            .tooltip(tip("Open the call", Some("⌘⇧J")))
            .on_click(cx.listener(move |this, _, _, cx| this.call_go(card_key.clone(), &card_title, cx)))
            .children(waiting.map(|(ring, from)| self.call_strip(&ring, &from, cx)))
            .children(share_strip)
            .child(self.call_title(call, subject, words, window, cx))
            .child(controls)
            .children(self.call_popover(call, subject, Place::Dock, window, cx))
    }

    /// Someone is calling: the same card with a `positive` edge and two equal buttons. It holds
    /// nothing that can take keyboard focus.
    fn call_incoming(&self, call: &Call, subject: &Subject, from: &str, window: &Window, cx: &mut Context<Self>) -> Div {
        let words = incoming_subtitle(call, subject.kind, from, subject.occupied);
        let support = self
            .sidebar_state
            .find(&self.trees, &call.conversation)
            .map(|c| c.call)
            .unwrap_or(protocol::CallSupport::Native);
        let accept_label = incoming_accept_label(call, support);
        let accept_icon = if call.incoming_video { "icons/video.svg" } else { "icons/phone.svg" };
        let decline = worded(
            "call-decline",
            Some("icons/phone-off.svg"),
            "Decline",
            theme::danger_soft(),
            theme::danger(),
        )
        .flex_1()
        .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
        .tooltip(tip("Decline", Some("⌘⇧R")))
        .on_click(cx.listener(|this, _, _, cx| this.call_decline(cx)));
        let conv_key = call.conversation.clone();
        let handoff = support == protocol::CallSupport::Handoff;
        let accept = worded(
            "call-accept",
            Some(accept_icon),
            accept_label,
            theme::positive(),
            theme::on_accent(),
        )
        .flex_1()
        .tooltip(tip("Accept", Some("⌘⇧A")))
        .on_click(cx.listener(move |this, _, _, cx| {
            if handoff {
                this.call_handoff(conv_key.clone(), cx);
            } else {
                this.call_accept(cx);
            }
        }));
        card()
            .border_color(Rgba { alpha: 0.45, ..theme::positive() })
            .child(self.call_title(call, subject, words, window, cx))
            .child(
                div()
                    .min_h(px(32.))
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .child(decline.min_w(px(100.)))
                    .child(accept.min_w(px(100.))),
            )
    }

    /// A call that did not happen, said in words. It stays until dismissed or the next call.
    fn call_failed(
        &self,
        call: &Call,
        subject: &Subject,
        reason: CallFailure,
        detail: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        let Failure { title, line, icon, acts } = failure(call, subject, reason, detail);
        // What the hand-off just said wins over the reason: it is the newer news.
        let reason_line = self.call_state.error.clone().unwrap_or(line);
        let network = call.conversation.network;
        let key = call.conversation.clone();

        let buttons = acts.into_iter().map(|act| {
            let key: Key = key.clone();
            let name = subject.title.clone();
            match act {
                Act::Again(word) => plain("call-again")
                    .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                    .child(word),
                Act::Message => plain("call-message")
                    .on_click(cx.listener(move |this, _, _, cx| this.call_go(key.clone(), &name, cx)))
                    .child("Message"),
                Act::Handoff => outlined("call-handoff", network)
                    .on_click(cx.listener(move |this, _, _, cx| this.call_handoff(key.clone(), cx))),
                Act::Settings => plain("call-settings")
                    .on_click(cx.listener(|_, _, _, cx| {
                        if !SETTINGS.is_empty() {
                            cx.background_executor().spawn(async { open(SETTINGS) }).detach();
                        }
                    }))
                    .child("Open System Settings"),
            }
        });
        let dismiss = div()
            .id("call-dismiss")
            .group("call-dismiss")
            .flex_none()
            .size(px(20.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(theme::text_faint())
            .hover(|style| style.bg(theme::selected()).text_color(theme::text()))
            .tooltip(tip("Dismiss", Some("⌘⇧H")))
            .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)))
            .child(glyph_in("icons/x.svg", 14., theme::text_faint(), Some(("call-dismiss", theme::text()))));
        card()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(
                        div()
                            .size(px(28.))
                            .flex_none()
                            .rounded(px(14.))
                            .bg(theme::warning_soft())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph(icon, 14., theme::warning())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .whitespace_normal()
                                    .line_clamp(2)
                                    .type_ui()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .whitespace_normal()
                                    .line_clamp(4)
                                    .type_small()
                                    .text_color(theme::text_soft())
                                    .child(reason_line),
                            ),
                    )
                    .child(dismiss),
            )
            .child(div().flex().flex_wrap().gap(px(4.)).children(buttons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(joined: i64) -> Call {
        Call {
            conversation: Key { network: Network::Discord, account: "1".into(), id: "900".into() },
            state: CallState::Connecting,
            participants: Vec::new(),
            muted: false,
            deafened: false,
            server_muted: false,
            server_deafened: false,
            suppressed: false,
            joined,
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
        ringing: Vec::new(),
        moved_from: None,
        move_notice: None,
        activity: None,
        stage_topic: String::new(),
        stage: false,
        request_to_speak: false,
        raised: Vec::new(),
        effect: None,
        incoming_video: false,
        title: String::new(),
        viewers: Vec::new(),
        stream_paused: false,
        stream_security: None,
        hand_raised: false,
        reaction: None,
        invite_link: None,
        join_muted: false,
        can_change_join_muted: false,
        participants_count: 0,
        joined_as: None,
        record_start_date: None,
        record_video_active: false,
        can_start_video: true,
        unmuted_video_count: 0,
        unmuted_video_limit: 0,
        rtmp_stream: false,
        rtmp_state: None,
        rtmp_quality: None,
        rtmp_qualities: Vec::new(),
        }
    }

    fn mara() -> Subject {
        Subject { title: "Mara Okafor".into(), kind: ConversationKind::Direct, avatar: None, space: None, occupied: 0 }
    }

    fn said(reason: CallFailure, detail: &str) -> Failure {
        failure(&call(0), &mara(), reason, detail)
    }

    #[test]
    fn a_ring_nobody_took_has_one_wording_and_a_decline_has_its_own() {
        // Whoever noticed (the adapter, or chatd's minute running out), the card says the same.
        for detail in ["Nobody picked up.", "", "No answer."] {
            let card = said(CallFailure::NoAnswer, detail);
            assert_eq!((card.title.as_str(), card.line.as_str()), ("No answer", "Mara Okafor didn't pick up."));
            assert_eq!(card.acts, [Act::Again("Call again"), Act::Message]);
        }
        let card = said(CallFailure::NoAnswer, "They declined the call.");
        assert_eq!((card.title.as_str(), card.line.as_str()), ("Declined", "Mara Okafor declined the call."));
        assert_eq!(card.acts, [Act::Message], "nobody is offered to ring again someone who said no");
    }

    #[test]
    fn each_audio_trouble_is_named_for_what_it_is() {
        let titles: Vec<String> = [
            "Allow it in System Settings › Privacy & Security › Microphone, then call again.",
            "Plug in a microphone, or choose another in Microphone and speaker.",
            "Plug in headphones or a speaker, or choose another in Microphone and speaker.",
            "The microphone didn't start. Reconnect it or choose another in Microphone and speaker, then try again.",
            "The speaker didn't start. Reconnect it or choose another in Microphone and speaker, then try again.",
        ]
        .iter()
        .map(|detail| said(CallFailure::Audio, detail).title)
        .collect();
        assert_eq!(
            titles,
            [
                "tinyclient can't use the microphone",
                "No microphone found",
                "No speaker found",
                "The microphone didn't start",
                "The speaker didn't start",
            ]
        );
        let speaker = said(CallFailure::Audio, "The speaker didn't start. Reconnect it or choose another in Microphone and speaker, then try again.");
        assert_eq!(speaker.line, "Reconnect it or choose another in Microphone and speaker, then try again.");
        assert_eq!(said(CallFailure::Audio, "").title, "Couldn't start audio");
    }

    #[test]
    fn a_call_that_was_up_is_lost_and_one_that_never_was_could_not_connect() {
        assert_eq!(said(CallFailure::Lost, "").title, "Couldn't connect");
        assert_eq!(failure(&call(1_700_000_000), &mara(), CallFailure::Lost, "").title, "Connection lost");
        assert_eq!(failure(&call(1_700_000_000), &mara(), CallFailure::Refused, "").title, "You're out of the call");
        let engine = said(CallFailure::Engine, "");
        assert_eq!((engine.title.as_str(), engine.line.as_str()), ("Couldn't connect", "The call engine stopped."));
        assert_eq!(said(CallFailure::Encryption, "").acts, [Act::Handoff]);
    }

    #[test]
    fn compact_place_prefers_the_server_title_over_the_raw_host() {
        assert_eq!(
            compact_place_label("General", Some("Tiny Client Official Server"), "private.tinyclient.io"),
            "General / Tiny Client Official Server"
        );
        assert_eq!(
            compact_place_label("General", Some("  "), "private.tinyclient.io"),
            "General / private.tinyclient.io",
            "a blank title falls back to the account name"
        );
        assert_eq!(compact_place_label("General", None, "private.tinyclient.io"), "General / private.tinyclient.io");
        assert_eq!(compact_place_label("General", None, "  "), "General");
    }

    #[test]
    fn a_question_names_both_calls_and_keeps_the_safe_answer_first() {
        let call = Confirm { conversation: call(0).conversation, title: "Mara Okafor".into(), answer: false };
        let asked = question(&call, "Lounge");
        assert_eq!(asked.title, "Leave Lounge and call Mara Okafor?");
        assert_eq!((asked.stay.as_str(), asked.go), ("Stay in Lounge", "Leave & call"));
        let ring = Confirm { answer: true, title: "Iris Navarro".into(), ..call };
        assert_eq!(question(&ring, "Lounge").title, "Leave Lounge and answer Iris Navarro?");
        assert_eq!(question(&ring, "Lounge").go, "Leave & answer");
    }

    fn tiny(joined: i64) -> Call {
        Call { conversation: Key { network: Network::Tiny, account: "night".into(), id: "lounge".into() }, ..call(joined) }
    }

    fn lounge() -> Subject {
        Subject { title: "Lounge".into(), kind: ConversationKind::Voice, avatar: None, space: Some("Night Shift".into()), occupied: 0 }
    }

    #[test]
    fn a_tiny_server_says_who_put_me_out_and_whether_i_may_come_back() {
        let said = |reason, detail: &str| failure(&tiny(1_700_000_000), &lounge(), reason, detail);
        let kicked = said(CallFailure::Refused, "You were removed from the call.");
        assert_eq!((kicked.title.as_str(), kicked.acts.as_slice()), ("Removed from the call", &[Act::Again("Rejoin")][..]));
        assert_eq!(kicked.line, "You were removed from the call.");
        assert!(said(CallFailure::Refused, "You were banned from this server.").acts.is_empty(), "no way back from a ban");
        assert_eq!(said(CallFailure::Refused, "You were timed out on this server, so you've left the call.").title, "Timed out");
        assert_eq!(said(CallFailure::Refused, "The room was deleted, so the call ended.").title, "Lounge was deleted");
        assert_eq!(said(CallFailure::Refused, "You joined a call on another device.").acts, [Act::Again("Join here")]);
        let lost = said(CallFailure::Lost, "The server stopped hearing this call's audio, so you've left the call.");
        assert_eq!((lost.title.as_str(), lost.acts.as_slice()), ("Connection lost", &[Act::Again("Rejoin")][..]));
        let never = failure(&tiny(0), &lounge(), CallFailure::Lost, "");
        assert_eq!(never.title, "Couldn't connect");
        for reason in [CallFailure::Refused, CallFailure::Lost, CallFailure::Engine, CallFailure::Audio, CallFailure::Encryption] {
            let card = failure(&tiny(0), &lounge(), reason, "");
            assert!(!card.acts.contains(&Act::Handoff), "a Tiny server has no app to hand off to: {reason:?}");
        }
    }

    #[test]
    fn the_narrowest_sidebar_gives_the_server_name_its_own_line() {
        // Sidebar 260, minus the dock inset and the card padding. The server line is this wide
        // because signal, lock and people sit on a later row.
        let inner = card_inner(crate::gui::layout::SIDEBAR_RANGE.0);
        assert!(inner >= 200., "{inner}");
    }

    #[test]
    fn durations_read_as_a_clock() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(724), "12:04");
        assert_eq!(clock(3723), "1:02:03");
    }

    #[test]
    fn dock_mode_avoids_duplicating_stage_bar_and_tiny_user_panel_controls() {
        // Direct/group/Discord call while viewing the stage: compact card (no duplicate buttons).
        assert_eq!(dock_mode(false, false, true), DockMode::CompactStage);
        // Direct/group/Discord call while browsing another chat: full dock controls.
        assert_eq!(dock_mode(false, false, false), DockMode::Full);
        // Tiny voice room on the Tiny server column (user panel present): compact Tiny card.
        assert_eq!(dock_mode(true, true, true), DockMode::CompactTiny);
        assert_eq!(dock_mode(true, true, false), DockMode::CompactTiny);
        // Tiny voice room when switched to Inbox (user panel absent): full dock controls if not viewing stage.
        assert_eq!(dock_mode(true, false, false), DockMode::Full);
        assert_eq!(dock_mode(true, false, true), DockMode::CompactStage);
    }
}
