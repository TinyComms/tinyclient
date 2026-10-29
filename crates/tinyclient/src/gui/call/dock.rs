//! The call dock: one card at the bottom of the sidebar that is every state of a call. Ringing
//! out, ringing in, connecting, in a call and failed are the same card with different children.
//! Over a Tiny server's own column, whose user panel holds mute and deafen, a call that is up is
//! the compact "Voice connected" bar instead: where, how well, and Disconnect.

use super::health::{self, Grade};
use super::people::{self, ringed};
use super::security::Place;
use super::{
    ensure_daemon, ensure_keys, handoff_label, icon as glyph_in, in_call, open, Confirm, Ended, Menu,
    Subject,
};
use crate::gui::components::{glyph, network_icon, tip, Avatar};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, Div, FontWeight, Rgba, Stateful,
};
use protocol::{Call, CallFailure, CallState, ConversationKind, Key, Network};

/// The bar: two rows and the padding around them.
const CARD: f32 = 96.;
/// The compact bar over a Tiny server's user panel: two lines of words.
const COMPACT: f32 = 54.;
const SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

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

/// How a 40 x 32 control looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Rest,
    /// My mic or audio is off.
    Off,
    /// Its menu is open.
    Open,
}

/// One control of the bar: 40 x 32, radius 8, a 16 px glyph. The card is `raised` already, so
/// a control at rest takes the white wash that reads as raised on it.
fn control(id: &'static str, icon: &'static str, look: Look, enabled: bool) -> Stateful<Div> {
    let (fill, color) = match look {
        Look::Rest => (theme::hover(), theme::text_soft()),
        Look::Off => (theme::danger_soft(), theme::danger()),
        Look::Open => (theme::selected(), theme::text()),
    };
    // Only a live control at rest brightens its glyph under the pointer.
    let lit = (enabled && look == Look::Rest).then(|| (id, theme::text()));
    div()
        .id(id)
        .group(id)
        .flex_none()
        .w(px(40.))
        .h(px(32.))
        .rounded(px(8.))
        .tab_index(0)
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
            (true, _) => control.cursor_pointer(),
        })
}

/// Leave, 48 x 32 at the right edge. It is live in every state that has it.
fn leave() -> Stateful<Div> {
    let id = "call-leave";
    div()
        .id(id)
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

/// A 32 px button with words: Cancel, Decline, Accept.
fn worded(id: &'static str, icon: Option<&'static str>, word: &'static str, fill: Rgba, color: Rgba) -> Stateful<Div> {
    // The red ones fill on hover, and their glyph goes white with the word.
    let lit = (color == theme::danger()).then(|| (id, theme::on_accent()));
    div()
        .id(id)
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
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .children(icon.map(|icon| glyph_in(icon, 14., color, lit)))
        .child(word)
}

/// A button of the failure card: equal widths, 28 high.
fn plain(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h(px(28.))
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
        .text_size(px(12.5))
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
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_soft())
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
        .child(network_icon(network, 14.))
        .child(div().min_w_0().truncate().child(handoff_label(network)))
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

fn question(confirm: &Confirm, here: &str) -> Question {
    let verb = if confirm.answer { "answer" } else { "call" };
    Question {
        title: format!("Leave {here} and {verb} {}?", confirm.title),
        stay: format!("Stay in {here}"),
        go: if confirm.answer { "Leave & answer" } else { "Leave & call" },
    }
}

impl Shell {
    /// The dock, for the end of the sidebar column as a flex-none child: full width, 112 high
    /// while a call is up (a failure is as tall as its words). `None` when there is no call.
    pub(in crate::gui) fn call_dock(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Div> {
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
                    (CallState::Failed { reason, detail }, _) => self.call_failed(&call, &subject, *reason, detail, cx),
                    (CallState::RingingIn { from }, _) => self.call_incoming(&call, &subject, from, cx),
                    (_, Some(confirm)) => self.call_question(&question(confirm, &subject.title), cx),
                    _ if self.tiny_column_has(&call.conversation) => self.call_compact(&call, &subject, cx),
                    _ => self.call_bar(&call, &subject, cx),
                }
            }
            None => self.call_ended(ended?, cx),
        };
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
                .child(card),
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
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .cursor_pointer()
            .hover(|style| style.bg(theme::selected()))
            .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)))
            .child("Dismiss");
        let title = div()
            .h(px(36.))
            .flex_none()
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
                            .truncate()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(subject.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(px(11.5))
                            .line_height(px(15.))
                            .text_color(theme::text_soft())
                            .child(network_icon(network, 11.))
                            .child(format!("Call ended · {}", clock(ended.seconds))),
                    ),
            );
        card()
            .h(px(CARD))
            .child(title)
            .child(div().h(px(32.)).flex_none().flex().items_center().child(div().flex_1()).child(dismiss))
    }

    /// "Leave Lounge and call Mara Okafor?": asked in the card, where the call is, before a
    /// call is left for another.
    fn call_question(&self, question: &Question, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, word: String| {
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .h(px(32.))
                .px(px(8.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(div().min_w_0().truncate().child(word))
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
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(question.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(theme::text_soft())
                            .child("You can be in one call at a time."),
                    ),
            )
            .child(div().flex().gap(px(4.)).child(stay).child(go))
    }

    /// Someone is calling while a call is up: a 40 px strip on top of the bar, with who and
    /// where, Decline and Accept. Accept asks before it leaves the call that is up.
    // CONTRACT: chatd holds one call and drops a ring that arrives during it, so nothing ever
    // fills this outside `--shot`. When `Status` can carry a second ring, pass it here.
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
            .h(px(40.))
            .flex_none()
            // Out to the card's edges, over its padding.
            .mt(px(-9.))
            .mx(px(-9.))
            .pl(px(10.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(theme::hairline())
            .text_size(px(12.))
            .text_color(theme::text_soft())
            .child(Avatar::named(from, 20.).build())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap(px(4.))
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(from.to_owned()))
                    .child(div().min_w_0().truncate().child(if from == ring.title {
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

    /// Over a Tiny server's column: "Voice connected", where, how well, and Disconnect. Mute,
    /// deafen and the voice settings are in the user panel right under it.
    fn call_compact(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let (state, color) = match call.state {
            CallState::Joined if self.call_muted_talking(call) => ("You're muted", theme::danger()),
            // Coloured by the link, as the bars are: green, amber when fair, red when poor.
            CallState::Joined => match self.tiny_health().map(|health| health.grade()) {
                Some(grade @ (Grade::Fair | Grade::Poor)) => ("Voice connected", grade.color()),
                _ => ("Voice connected", theme::positive()),
            },
            CallState::Reconnecting => ("Reconnecting…", theme::warning()),
            _ => ("Connecting…", theme::warning()),
        };
        let notice = self.call_state.notice.clone().filter(|_| in_call(call));
        let clock = if notice.is_some() || call.state == CallState::Joined { None } else { self.call_counter(call, cx) };
        let place = match &subject.space {
            Some(server) => format!("{} / {server}", subject.title),
            None => subject.title.clone(),
        };
        let key = call.conversation.clone();
        let title = subject.title.clone();
        let words = div()
            .id("call-compact-where")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(1.))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .tooltip(tip(format!("Go to {}", subject.title), Some("⌘⇧J")))
            .on_click(cx.listener(move |this, _, _, cx| this.call_go(key.clone(), &title, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .when(self.call_muted_talking(call), |row| row.child(glyph("icons/mic-off.svg", 13., theme::danger())))
                    .child(div().flex_none().child(state))
                    .children(clock.map(|clock| div().flex_none().font_weight(FontWeight::NORMAL).text_color(theme::text_soft()).child(clock))),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(11.5))
                    .line_height(px(15.))
                    .text_color(if notice.is_some() { theme::warning() } else { theme::text_soft() })
                    .child(notice.unwrap_or(place)),
            );
        let disconnect = leave()
            .w(px(36.))
            .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)));
        card()
            .h(px(COMPACT))
            .flex_row()
            .items_center()
            .gap(px(8.))
            .children(self.call_signal(call, cx))
            .child(words)
            .children(self.call_security(call, Place::TinyBar, cx))
            .child(disconnect)
    }

    /// Row 1, 36 px: who or where, the state in words, and who else is there. A click on it
    /// opens the call's conversation.
    fn call_title(&self, call: &Call, subject: &Subject, words: String, cx: &mut Context<Self>) -> Stateful<Div> {
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
            let peer_speaks = live && direct && call.participants.first().is_some_and(|peer| peer.speaking);
            let picture = self.media_path(&call.conversation, subject.avatar.as_ref());
            let avatar = Avatar::new(&call.conversation, &subject.title, 28.).image_at(picture);
            ringed(avatar.build(), 28., peer_speaks)
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
        let words = if talking { format!("You're muted. Press {} to talk", crate::gui::shortcuts::label("⌘⇧M")) } else { words };
        let counter = if refused.is_some() || notice.is_some() { None } else { self.call_counter(call, cx) };
        let refused = refused.or(notice);
        let sub = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .text_size(px(11.5))
            .line_height(px(15.))
            .text_color(color)
            .child(network_icon(network, 11.))
            .child(div().min_w_0().truncate().child(refused.unwrap_or(words)))
            .children(counter)
            .children(self.call_switch_device(call, live, cx));
        let right: Option<AnyElement> = match (live, direct) {
            (false, _) => None,
            (true, true) => call
                .participants
                .first()
                .filter(|peer| peer.muted)
                .map(|_| glyph("icons/mic-off.svg", 13., theme::text_faint()).into_any_element()),
            (true, false) if call.participants.is_empty() => None,
            (true, false) => Some(
                people::stack(call)
                    .id("call-stack")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_menu_toggle(Menu::People, cx);
                    }))
                    .into_any_element(),
            ),
        };
        let key = call.conversation.clone();
        let title = subject.title.clone();
        let signal = self.call_signal(call, cx);
        let security = self.call_security(call, Place::Dock, cx);
        div()
            .id("call-title")
            .h(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.call_go(key.clone(), &title, cx)))
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
                            .truncate()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(subject.title.clone()),
                    )
                    .child(sub),
            )
            .children(signal)
            .children(security)
            .children(right)
    }

    /// Ringing out, connecting, joined and reconnecting: the 96 px bar.
    fn call_bar(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let place = subject.space.as_deref().unwrap_or(call.conversation.network.label());
        let alone = self.call_left_alone(call);
        let words = match (&call.state, alone.as_deref()) {
            (CallState::RingingOut, _) => "Ringing…".to_owned(),
            (CallState::Connecting, _) => "Connecting…".to_owned(),
            (CallState::Reconnecting, _) => "Reconnecting…".to_owned(),
            // Alone in a direct call: said, and left to me. tinyclient does not hang up for me.
            (_, Some(peer)) if subject.kind == ConversationKind::Direct => {
                format!("{} left ·", peer.split_whitespace().next().unwrap_or(peer))
            }
            _ => format!("{place} ·"),
        };
        let joined = call.state == CallState::Joined;
        // Reconnecting keeps the joined controls, dead; only Leave is live.
        let enabled = call.state != CallState::Reconnecting;
        let menu = self.call_menu().filter(|_| enabled);
        let off = call.muted || call.deafened;

        let mute = control(
            "call-mute",
            if off { "icons/mic-off.svg" } else { "icons/mic.svg" },
            if off { Look::Off } else { Look::Rest },
            enabled,
        )
        .when(enabled, |mute| {
            mute.tooltip(tip(if off { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
                .on_click(cx.listener(|this, _, _, cx| this.call_mute(cx)))
        })
        // While I am heard: a 2 px ring inside the button, the same green as a speaker's.
        .when(call.speaking && !off && joined, |mute| {
            mute.relative().child(
                div().absolute().top_0().left_0().size_full().rounded(px(8.)).border_2().border_color(theme::positive()),
            )
        });
        let deafen = control(
            "call-deafen",
            if call.deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" },
            if call.deafened { Look::Off } else { Look::Rest },
            enabled,
        )
        .when(enabled, |deafen| {
            deafen
                .tooltip(tip(if call.deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
                .on_click(cx.listener(|this, _, _, cx| this.call_deafen(cx)))
        });
        let devices = control(
            "call-devices",
            "icons/settings.svg",
            if menu == Some(Menu::Devices) { Look::Open } else { Look::Rest },
            enabled,
        )
        .when(enabled, |devices| {
            devices
                .tooltip(tip("Microphone and speaker", None))
                .on_click(cx.listener(|this, _, _, cx| this.call_menu_toggle(Menu::Devices, cx)))
        });
        let end = if in_call(call) {
            leave()
        } else {
            worded("call-cancel", None, "Cancel", theme::danger_soft(), theme::danger())
                .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
                .tooltip(tip("Cancel", Some("⌘⇧H")))
        };
        let controls = div()
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(mute)
            // Deafen is for a call that is up; before that only mute and devices can be preset.
            .when(in_call(call), |row| row.child(deafen))
            .child(devices)
            .child(div().flex_1())
            .child(end.on_click(cx.listener(|this, _, _, cx| this.call_leave(cx))));

        let popover = match menu {
            Some(Menu::Devices) => Some((300., self.call_devices_menu(call, cx).p(px(6.)))),
            Some(Menu::People) if joined => Some((280., people::list(call, subject).py(px(6.)))),
            _ => None,
        };
        #[cfg(feature = "gui-shot")]
        let strip = super::shot::strip();
        #[cfg(not(feature = "gui-shot"))]
        let strip: Option<(Confirm, String)> = None;
        let lifted = if strip.is_some() { 40. } else { 0. };
        card()
            .h(px(CARD + lifted))
            .children(strip.map(|(ring, from)| self.call_strip(&ring, &from, cx)))
            .child(self.call_title(call, subject, words, cx))
            .child(controls)
            .children(popover.map(|(width, body)| {
                // Above the card, level with its left edge, on top of whatever the list shows.
                deferred(
                    div()
                        .id("call-popover")
                        .absolute()
                        .left(px(-1.))
                        .bottom(px(CARD + lifted + 5.))
                        .w(px(width))
                        .rounded(px(12.))
                        .bg(theme::overlay())
                        .border_1()
                        .border_color(theme::border())
                        .shadow(theme::shadow_popover())
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.call_menu_close(cx)))
                        .child(body),
                )
                .with_priority(1)
            }))
    }

    /// Someone is calling: the same card with a `positive` edge and two equal buttons. It holds
    /// nothing that can take keyboard focus.
    fn call_incoming(&self, call: &Call, subject: &Subject, from: &str, cx: &mut Context<Self>) -> Div {
        let words = match subject.kind {
            ConversationKind::Direct => "Incoming call".to_owned(),
            _ => format!("{from} is calling"),
        };
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
        let accept = worded(
            "call-accept",
            Some("icons/phone.svg"),
            "Accept",
            theme::positive(),
            theme::on_accent(),
        )
        .flex_1()
        .tooltip(tip("Accept", Some("⌘⇧A")))
        .on_click(cx.listener(|this, _, _, cx| this.call_accept(cx)));
        card()
            .h(px(CARD))
            .border_color(Rgba { a: 0.45, ..theme::positive() })
            .child(self.call_title(call, subject, words, cx))
            .child(div().h(px(32.)).flex_none().flex().gap(px(4.)).child(decline).child(accept))
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
                        cx.background_executor().spawn(async { open(SETTINGS) }).detach();
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
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(16.))
                                    .text_color(theme::text_soft())
                                    .child(reason_line),
                            ),
                    )
                    .child(dismiss),
            )
            .child(div().flex().gap(px(4.)).children(buttons))
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
        }
    }

    fn mara() -> Subject {
        Subject { title: "Mara Okafor".into(), kind: ConversationKind::Direct, avatar: None, space: None }
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
        Subject { title: "Lounge".into(), kind: ConversationKind::Voice, avatar: None, space: Some("Night Shift".into()) }
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
    fn durations_read_as_a_clock() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(724), "12:04");
        assert_eq!(clock(3723), "1:02:03");
    }
}
