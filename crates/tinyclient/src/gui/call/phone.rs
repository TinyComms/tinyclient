//! A call on a phone (`docs/design/07-calls.md`, at a phone's size). The desktop keeps every state
//! of a call in one card in the sidebar; a phone has no sidebar beside the conversation, so a
//! call is a screen of its own instead: the people in it as a grid of tiles, a screen share on
//! the stage, my camera in a corner, and the controls in a thumb-sized bar at the bottom. Going
//! back from it leaves the call running as a bar over every other screen (the desktop's call
//! bar, at the top), which a tap brings back.
//!
//! The same states drive it as drive the dock: `Call::state` from chatd, `call_state.ended`
//! and `call_state.local` from this window. It is drawn only below `layout::PHONE`.
//!
//! - `stage`: the tiles, the screen share, my camera's corner tile, one person large.
//! - `controls`: the bottom bar and its round buttons.
//! - `screens`: ringing in, failed and ended.
//! - `sheets`: the people page, the audio sheet, More, the security panel, the camera's
//!   permission.
//! - `hooks`: what the platform does for the call (system settings, audio route, camera).

mod controls;
pub(crate) mod hooks;
mod screens;
mod sheets;
mod stage;

use super::{in_call, Ended, Subject};
use crate::gui::components::{glyph, network_icon, Avatar};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, ElementId, MouseButton, Rgba, Stateful, Window};
use protocol::{Call, CallState, Content, ConversationKind, Direction, Key, Network};

/// The strip under the status bar: minimise, where the call is, the people.
/// A redraw for my own camera's picture is already on its way.
static SELF_REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const HEADER: f32 = 56.;
/// The bar a minimised call draws over every screen, under the status bar.
const BAR: f32 = 56.;
/// A control: its round button, the gap and its label.
const BUTTON: f32 = 58.;
const CONTROL_ROW: f32 = BUTTON + 6. + 16.;
const CONTROL_GAP: f32 = 14.;

/// A sheet over the call screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Sheet {
    /// Everything that is not in the bar: people, audio, push to talk, noise, security.
    More,
    /// Where the sound plays and which microphone hears me.
    Devices,
    /// How the call is protected.
    Security,
    /// The camera was refused: the way to the system settings.
    Camera,
}

/// Which corner my camera's tile sits in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) enum Corner {
    #[default]
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

/// My camera's tile being dragged: where the finger went down, and how far it has moved.
#[derive(Clone, Copy, Debug)]
pub(in crate::gui) struct Drag {
    from: (f32, f32),
    by: (f32, f32),
}

/// What the call screen keeps between frames.
#[derive(Default)]
pub(in crate::gui) struct State {
    /// Gone back from the screen: the call is the bar over every other screen.
    pub minimised: bool,
    /// The people page is pushed over the screen.
    pub people: bool,
    pub sheet: Option<Sheet>,
    pub corner: Corner,
    pub drag: Option<Drag>,
    /// The camera faces away from me.
    pub back_camera: bool,
    /// Accepted with video: the camera goes on once the call is up.
    pub video_on_join: bool,
}

impl State {
    /// The call changed state or place. A new call, a ring and a failure come up full-screen;
    /// a call that is over takes its page and sheets with it.
    pub(in crate::gui) fn state_changed(&mut self, call: Option<&Call>, before: Option<&Key>) {
        match call {
            None => {
                self.people = false;
                self.sheet = None;
                self.drag = None;
                self.video_on_join = false;
                hooks::forget_route();
            }
            Some(call) => {
                let new = before != Some(&call.conversation);
                if new || matches!(call.state, CallState::RingingIn { .. } | CallState::Failed { .. }) {
                    self.minimised = false;
                }
                if !in_call(call) {
                    self.people = false;
                    if self.sheet != Some(Sheet::Devices) {
                        self.sheet = None;
                    }
                }
            }
        }
    }
}

/// How a call shows on a phone now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum View {
    /// The whole screen.
    Screen,
    /// The bar over every other screen.
    Bar,
}

/// The shots name the view, page and sheet (`shot::phone_view`).
#[cfg(feature = "gui-shot")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Shown {
    Screen,
    Bar,
    People,
    Sheet(Sheet),
}

/// Our own encode of this picture is on.
pub(super) fn sending(call: &Call, content: Content) -> bool {
    call.video.iter().any(|stream| stream.direction == Direction::Encode && stream.content == content)
}

/// The call can carry pictures at all: a Tiny room's call (07 §6). Elsewhere camera and share
/// are absent, not greyed.
fn pictures_network(call: &Call) -> bool {
    call.conversation.network == Network::Tiny
}

/// One round control: its look, and the colors that go with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Look {
    Rest,
    /// Share or camera is on, the speaker is the route.
    On,
    /// My mic or my sound is off.
    Off,
    /// Leave, cancel, decline.
    End,
    Accept,
    /// Not possible now; its label says why.
    Unavailable,
}

impl Look {
    fn colors(self) -> (Rgba, Rgba, Rgba) {
        // fill, glyph, label
        match self {
            Look::Rest => (theme::selected(), theme::text(), theme::text_soft()),
            Look::On => (theme::text(), theme::canvas(), theme::text()),
            Look::Off => (theme::danger_soft(), theme::danger(), theme::danger()),
            Look::End => (theme::danger_fill(), theme::on_accent(), theme::text_soft()),
            Look::Accept => (theme::positive(), theme::on_accent(), theme::text_soft()),
            Look::Unavailable => (theme::hover(), theme::text_disabled(), theme::text_faint()),
        }
    }
}

/// A round button `size` across with a glyph in it, and its label under it.
pub(super) fn round(id: impl Into<ElementId>, icon: &'static str, label: impl Into<gpui::SharedString>, look: Look, size: f32) -> Stateful<Div> {
    let (fill, color, words) = look.colors();
    let pressed = match look {
        Look::End => Rgba { alpha: 0.8, ..theme::danger_fill() },
        Look::Accept => Rgba { alpha: 0.8, ..theme::positive() },
        Look::On => theme::text_soft(),
        _ => theme::pressed(),
    };
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .id("disc")
                .size(px(size))
                .flex_none()
                .rounded_full()
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .when(look != Look::Unavailable, |button| button.active(move |style| style.bg(pressed)))
                .child(glyph(icon, (size * 0.42).round(), color)),
        )
        .child(
            div()
                .max_w_full()
                .truncate()
                .text_size(px(tokens::scaled(12.)))
                .line_height(px(16.))
                .font_weight(theme::MEDIUM)
                .text_color(words)
                .child(label.into()),
        )
}

/// A 44 pt glyph button for a strip.
pub(super) fn strip_button(id: impl Into<ElementId>, icon: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(tokens::TOUCH))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .active(|style| style.bg(theme::pressed()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph(icon, 22., theme::text()))
}

/// `12:04`, or `1:02:03` past the hour.
pub(super) fn clock(seconds: i64) -> String {
    match seconds / 3600 {
        0 => format!("{}:{:02}", seconds / 60, seconds % 60),
        hours => format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60),
    }
}

/// The first word of a name: "Mara".
pub(super) fn first(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

impl Shell {
    /// How the call shows now, and the call. `None`: no call, or not a phone.
    pub(in crate::gui) fn call_phone_view(&self, snapshot: &Snapshot) -> Option<View> {
        let call = self.call_shown(snapshot);
        #[cfg(feature = "gui-shot")]
        if let Some(shown) = super::shot::phone_view() {
            let has = call.is_some() || super::shot::ended().is_some();
            return has.then_some(if shown == Shown::Bar { View::Bar } else { View::Screen });
        }
        let ringing = call.as_ref().is_some_and(|call| matches!(call.state, CallState::RingingIn { .. }));
        if ringing {
            return Some(View::Screen);
        }
        (call.is_some() || self.call_state.ended.is_some()).then_some(if self.call_state.phone.minimised { View::Bar } else { View::Screen })
    }

    /// The people page is pushed over the call screen.
    fn call_phone_people_open(&self) -> bool {
        #[cfg(feature = "gui-shot")]
        if let Some(shown) = super::shot::phone_view() {
            return shown == Shown::People;
        }
        self.call_state.phone.people
    }

    /// The sheet open over the call screen.
    fn call_phone_sheet(&self) -> Option<Sheet> {
        #[cfg(feature = "gui-shot")]
        if let Some(shown) = super::shot::phone_view() {
            return match shown {
                Shown::Sheet(sheet) => Some(sheet),
                _ => None,
            };
        }
        self.call_state.phone.sheet
    }

    /// The call that ended, shown for a few seconds.
    fn call_phone_ended(&self) -> Option<Ended> {
        #[cfg(feature = "gui-shot")]
        if let Some(ended) = super::shot::ended() {
            return Some(ended);
        }
        self.call_state.ended.as_ref().map(|ended| Ended { conversation: ended.conversation.clone(), subject: ended.subject.clone(), seconds: ended.seconds })
    }

    /// Back to the call screen from anywhere: the bar's tap, the strip's glyph.
    pub(in crate::gui) fn call_phone_restore(&mut self, cx: &mut Context<Self>) {
        self.call_state.phone.minimised = false;
        cx.notify();
    }

    /// Down to the bar: the screen's chevron, the back gesture.
    pub(in crate::gui) fn call_phone_minimise(&mut self, cx: &mut Context<Self>) {
        self.call_phone_close_sheet(cx);
        self.call_state.phone.people = false;
        self.call_state.phone.minimised = true;
        cx.notify();
    }

    pub(super) fn call_phone_open_sheet(&mut self, sheet: Sheet, cx: &mut Context<Self>) {
        // The audio sheet's meters flow while it is open, as the desktop's device menu's do.
        let menu = (sheet == Sheet::Devices).then_some(super::Menu::Devices);
        self.call_menu_set(menu, cx);
        self.call_state.phone.sheet = Some(sheet);
        cx.notify();
    }

    pub(super) fn call_phone_close_sheet(&mut self, cx: &mut Context<Self>) {
        if self.call_state.phone.sheet.take().is_some() {
            self.call_menu_set(None, cx);
        }
        cx.notify();
    }

    /// Back on a phone while the call screen is up: a sheet, then the people page, then the
    /// screen itself goes down to the bar. False when the call screen is not showing.
    pub(in crate::gui) fn call_phone_back(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        if self.call_phone_view(snapshot) != Some(View::Screen) {
            return false;
        }
        if self.call_state.phone.sheet.is_some() {
            self.call_phone_close_sheet(cx);
        } else if self.call_state.phone.people {
            self.call_state.phone.people = false;
            cx.notify();
        } else if self.call_shown(snapshot).is_some_and(|call| matches!(call.state, CallState::RingingIn { .. })) {
            // A ring stays until it is answered or declined.
            return true;
        } else {
            self.call_phone_minimise(cx);
        }
        true
    }

    /// The ticking clock of the call screen and its bar.
    pub(super) fn call_phone_clock(&self, call: &Call, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.call_clock(&self.call_state.phone_counter, call, cx)
    }

    /// Who a call is with, or where, and the picture of each person in it.
    pub(super) fn call_phone_face(&self, call: &Call, subject: &Subject, id: &str) -> Option<std::path::PathBuf> {
        #[cfg(feature = "gui-shot")]
        if let Some(face) = super::shot::phone_face(id) {
            return Some(face);
        }
        if subject.kind == ConversationKind::Direct && call.participants.first().is_some_and(|peer| peer.id == id) {
            return self.media_path(&call.conversation, subject.avatar.as_ref());
        }
        if call.conversation.network == Network::Tiny {
            return self.tiny_page_facts().filter(|page| page.account == &call.conversation.account()).and_then(|page| page.picture(id));
        }
        None
    }

    /// My name and picture in a call.
    pub(super) fn call_phone_me(&self) -> (String, Option<std::path::PathBuf>) {
        #[cfg(feature = "gui-shot")]
        if let Some(me) = super::shot::phone_me() {
            return me;
        }
        let me = crate::gui::app::prefs::me();
        let name = if me.name.trim().is_empty() { "You".to_owned() } else { me.name.clone() };
        (name, me.picture_path())
    }

    /// The call screen, the whole phone, while it shows. `None` when there is no call or it is
    /// minimised to the bar.
    pub(in crate::gui) fn call_phone_screen(&self, snapshot: &Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.call_phone_view(snapshot)? != View::Screen {
            return None;
        }
        let size = window.viewport_size();
        let (wide, tall) = (f32::from(size.width), f32::from(size.height));
        // My own camera's picture changes with every frame the platform writes, not with chatd's
        // answers: draw again soon while there is one.
        if hooks::self_frame().is_some() && !SELF_REFRESH.swap(true, std::sync::atomic::Ordering::Relaxed) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                SELF_REFRESH.store(false, std::sync::atomic::Ordering::Relaxed);
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let body: AnyElement = match self.call_shown(snapshot) {
            Some(call) => {
                let subject = Subject::of(self, snapshot, &call);
                match &call.state {
                    CallState::RingingIn { from } => self.call_phone_incoming(&call, &subject, from, cx).into_any_element(),
                    CallState::Failed { reason, detail } => self.call_phone_failed(&call, &subject, *reason, detail, cx).into_any_element(),
                    _ => self.call_phone_live(&call, &subject, wide, tall, cx).into_any_element(),
                }
            }
            None => self.call_phone_over(&self.call_phone_ended()?, cx).into_any_element(),
        };
        Some(
            div()
                .id("call-phone")
                .size_full()
                .relative()
                .bg(theme::canvas())
                .text_color(theme::text())
                .occlude()
                .child(body)
                .into_any_element(),
        )
    }

    /// A call that is ringing out, connecting, up or reconnecting: header, stage, controls,
    /// then whatever sheet or page is over them.
    fn call_phone_live(&self, call: &Call, subject: &Subject, wide: f32, tall: f32, cx: &mut Context<Self>) -> Div {
        let rows = self.call_phone_control_rows(call, subject);
        let controls_h = 16. + rows as f32 * CONTROL_ROW + rows.saturating_sub(1) as f32 * CONTROL_GAP + tokens::safe_bottom() + 10.;
        let stage_h = (tall - tokens::safe_top() - HEADER - controls_h - 16.).max(120.);
        let stage_w = wide - 20.;
        let layout = self.call_phone_layout(call, subject);
        let hero = matches!(layout, stage::Layout::Hero);
        let sheet = self.call_phone_sheet().filter(|_| in_call(call) || matches!(self.call_phone_sheet(), Some(Sheet::Devices)));
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_none().h(px(tokens::safe_top())))
            .child(self.call_phone_header(call, subject, hero, cx))
            .children(self.call_phone_notice(call))
            .child(div().flex_1().min_h_0().px(px(10.)).pt(px(4.)).pb(px(12.)).child(self.call_phone_stage(call, subject, layout, stage_w, stage_h, cx)))
            .child(self.call_phone_controls(call, subject, cx))
            .children(self.call_phone_pip(call, layout, wide, tall, controls_h, cx))
            .children(self.call_phone_people_open().then(|| self.call_phone_people(call, subject, cx)))
            .children(sheet.map(|sheet| self.call_phone_sheet_view(sheet, call, subject, tall, cx)))
    }

    /// What was refused, or what the system just did, over the stage.
    fn call_phone_notice(&self, call: &Call) -> Option<Div> {
        let words = self.call_state.error.clone().or_else(|| self.call_state.notice.clone().filter(|_| in_call(call)))?;
        Some(
            div().flex_none().px(px(16.)).pb(px(6.)).flex().justify_center().child(
                div()
                    .max_w_full()
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(14.))
                    .bg(theme::warning_soft())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(glyph("icons/triangle-alert.svg", 14., theme::warning()))
                    .child(div().min_w_0().whitespace_normal().line_clamp(2).phone_preview().text_color(theme::warning()).child(words)),
            ),
        )
    }

    /// Minimise at the left, where the call is in the middle (its name, then its network, the
    /// clock, its protection and its link), the people at the right. A hero layout already
    /// shows the name large, so the middle is only the line under it.
    fn call_phone_header(&self, call: &Call, subject: &Subject, hero: bool, cx: &mut Context<Self>) -> Div {
        let network = call.conversation.network;
        let place = subject.space.clone().unwrap_or_else(|| network.label().to_owned());
        // A hero says the state under the name, with a ring's count: the header keeps to where.
        let (state, color) = match call.state {
            _ if self.call_muted_talking(call) => (Some("You're muted"), theme::danger()),
            _ if hero => (None, theme::text_soft()),
            CallState::RingingOut => (Some("Ringing…"), theme::text_soft()),
            CallState::Connecting => (Some("Connecting…"), theme::text_soft()),
            CallState::Reconnecting => (Some("Reconnecting…"), theme::warning()),
            _ => (None, theme::text_soft()),
        };
        let counted = if hero { call.state == CallState::Joined } else { call.state != CallState::Connecting };
        let clock = counted.then(|| self.call_phone_clock(call, cx)).flatten();
        let security = call.security.as_ref().map(|security| {
            let shown = super::security::badge(security, call.participants.is_empty());
            let (icon, tint) = super::security::badge_look(shown);
            div()
                .id("call-phone-security")
                .flex_none()
                .size(px(28.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .active(|style| style.bg(theme::pressed()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.call_phone_open_sheet(Sheet::Security, cx)))
                .child(glyph(icon, 14., tint))
        });
        let signal = (call.state == CallState::Joined && network == Network::Tiny)
            .then(|| self.tiny_health())
            .flatten()
            .map(|health| super::health::bars(health.grade(), 12.));
        let dot = || div().flex_none().text_color(theme::text_faint()).child("·");
        let line = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.))
            .phone_preview()
            .text_color(color)
            .child(div().flex_none().child(network_icon(network, 13.)))
            .child(div().min_w_0().truncate().text_color(theme::text_soft()).child(place))
            .children(state.map(|words| div().flex_none().flex().gap(px(5.)).child(dot()).child(words)))
            .children(clock.map(|clock| div().flex_none().flex().gap(px(5.)).child(dot()).child(clock)))
            .children(signal)
            .children(security);
        let middle = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .children((!hero).then(|| div().max_w_full().truncate().phone_title().child(subject.title.clone())))
            .child(line);
        let count = call.participants.len() + 1;
        let group = subject.kind != ConversationKind::Direct && in_call(call);
        let people = group.then(|| {
            div()
                .relative()
                .child(strip_button("call-phone-people", "icons/users.svg").on_click(cx.listener(|this, _, _, cx| {
                    this.call_state.phone.people = true;
                    cx.notify();
                })))
                .child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .right(px(2.))
                        .min_w(px(18.))
                        .h(px(18.))
                        .px(px(4.))
                        .rounded_full()
                        .bg(theme::overlay())
                        .border_1()
                        .border_color(theme::border())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .font_weight(theme::SEMIBOLD)
                        .text_color(theme::text())
                        .child(count.to_string()),
                )
        });
        div()
            .flex_none()
            .h(px(HEADER))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .child(strip_button("call-phone-minimise", "icons/chevron-down.svg").on_click(cx.listener(|this, _, _, cx| this.call_phone_minimise(cx))))
            .child(middle)
            .child(div().flex_none().w(px(tokens::TOUCH)).children(people))
    }

    /// The call, minimised: a bar over every screen with where it is, how long it has run and
    /// who is speaking; a tap goes back to it, mute and leave at its right. The status bar above
    /// it takes its tint, so the top of every screen says a call is up.
    pub(in crate::gui) fn call_phone_bar(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if self.call_phone_view(snapshot)? != View::Bar {
            return None;
        }
        let fill = theme::over(Rgba { alpha: 0.22, ..theme::positive() }, theme::sidebar());
        let base = div()
            .id("call-phone-bar")
            .w_full()
            .flex_none()
            .bg(fill)
            .border_b_1()
            .border_color(theme::hairline())
            .cursor_pointer()
            .child(div().h(px(tokens::status_bar())));
        let row = div().h(px(BAR)).pl(px(14.)).pr(px(8.)).flex().items_center().gap(px(10.));
        let Some(call) = self.call_shown(snapshot) else {
            // The other side ended it: said for a few seconds.
            let ended = self.call_phone_ended()?;
            let words = format!("Call ended · {}", clock(ended.seconds));
            return Some(
                base.on_click(cx.listener(|this, _, _, cx| this.call_leave(cx))).child(
                    row.child(self.call_phone_lead(&ended.conversation, &ended.subject, None, 32., false))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().phone_row().font_weight(theme::SEMIBOLD).child(ended.subject.title.clone()))
                                .child(div().truncate().phone_stamp().text_color(theme::text_soft()).child(words)),
                        )
                        .child(round("call-phone-bar-dismiss", "icons/x.svg", "", Look::Rest, 40.).flex_none().w(px(44.))),
                ),
            );
        };
        let subject = Subject::of(self, snapshot, &call);
        let talker = call.participants.iter().find(|who| who.speaking).map(|who| first(&who.name).to_owned());
        let anyone = talker.is_some() || (call.speaking && !call.muted);
        let (words, color): (String, Rgba) = match (&call.state, &talker) {
            (CallState::RingingOut, _) => ("Ringing…".into(), theme::text_soft()),
            (CallState::Connecting, _) => ("Connecting…".into(), theme::text_soft()),
            (CallState::Reconnecting, _) => ("Reconnecting…".into(), theme::warning()),
            (CallState::Failed { .. }, _) => ("The call ended".into(), theme::warning()),
            (_, Some(name)) => (format!("{name} is speaking"), theme::positive()),
            _ if call.muted => ("You're muted".into(), theme::danger()),
            _ => ("Tap to return to the call".into(), theme::text_soft()),
        };
        let clock = self.call_phone_clock(&call, cx);
        let off = call.muted || call.deafened;
        let peer_speaks = subject.kind == ConversationKind::Direct && talker.is_some();
        let lead = self.call_phone_lead(&call.conversation, &subject, call.participants.first().map(|peer| peer.id.as_str()).map(|id| self.call_phone_face(&call, &subject, id)).flatten(), 32., peer_speaks);
        let mute = round("call-phone-bar-mute", if off { "icons/mic-off.svg" } else { "icons/mic.svg" }, "", if off { Look::Off } else { Look::Rest }, 40.)
            .flex_none()
            .w(px(44.))
            .on_click(cx.listener(|this, _, _, cx| this.call_mute(cx)));
        let end = round("call-phone-bar-end", "icons/phone-off.svg", "", Look::End, 40.)
            .flex_none()
            .w(px(44.))
            .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)));
        Some(
            base.on_click(cx.listener(|this, _, _, cx| this.call_phone_restore(cx))).child(
                row.child(lead)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().min_w_0().truncate().phone_row().font_weight(theme::SEMIBOLD).child(subject.title.clone()))
                                    .children(clock.map(|clock| div().flex_none().phone_stamp().text_color(theme::positive()).child(clock))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .phone_stamp()
                                    .text_color(color)
                                    .child(glyph("icons/audio-lines.svg", 13., if anyone { theme::positive() } else { theme::text_faint() }))
                                    .child(div().min_w_0().truncate().child(words)),
                            ),
                    )
                    .children(in_call(&call).then_some(mute))
                    .child(end),
            ),
        )
    }

    /// The face of a call: the person's picture in its speaking ring, or a voice channel's plate.
    pub(super) fn call_phone_lead(&self, key: &Key, subject: &Subject, picture: Option<std::path::PathBuf>, size: f32, speaking: bool) -> Div {
        if subject.kind == ConversationKind::Voice || subject.kind == ConversationKind::Channel {
            return div()
                .size(px(size))
                .flex_none()
                .rounded(px((size * 0.28).round()))
                .bg(theme::positive_soft())
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("icons/volume-2.svg", (size * 0.5).round(), theme::positive()));
        }
        let picture = picture.or_else(|| self.media_path(key, subject.avatar.as_ref()));
        #[cfg(feature = "gui-shot")]
        let picture = picture.or_else(|| super::shot::phone_face(&subject.title.to_lowercase().replace(' ', "-")));
        super::ringed(Avatar::new(key, &subject.title, size).image_at(picture).build(), size, speaking)
    }
}
