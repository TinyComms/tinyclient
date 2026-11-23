//! The main-pane call view for any call (direct, group, Discord voice, or Tiny room), extracted
//! from `gui/tiny/channel.rs`. A Tiny room page wraps these same functions via `Stage::of_room`
//! and keeps room-only behaviour (empty/full states, presets, server-held mute, member sidebar).

use super::health::{self, Grade};
use super::security::Place;
use super::stage::{self, sending, sending_now, CallPeople, Focus, People, Seat, Stage, Stream};
use super::{watching, Menu, Subject};
use crate::gui::chrome::{self, End};
use crate::gui::components::{
    button, button_with_icon, glyph, icon_button, tip, Avatar, ButtonKind,
};
use crate::gui::layout::{drags_window, TOP_BAR};
use crate::gui::state::{Shell, Snapshot};
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Float;
use gpui::{
    deferred, div, img, prelude::*, px, relative, AnyElement, Context, Div, MouseButton,
    MouseDownEvent, ObjectFit, SharedString, Stateful, Window,
};
use protocol::{Call, CallState, Content, ConversationKind, Key, MediaRequest, Network, PictureKind, Request};
use std::collections::HashMap;

const TILE_W: f32 = 196.;
const TILE_H: f32 = 148.;
/// Two-person direct call: one large tile each.
const DUO_W: f32 = 280.;
const DUO_H: f32 = 210.;
/// A focused tile: the one person drawn large.
const FOCUS_W: f32 = 420.;
const FOCUS_H: f32 = 300.;
/// A tile in the strip under a focused stream.
const STRIP_W: f32 = 150.;
const STRIP_H: f32 = 96.;

/// Said under the call while echo cancellation is switched off in Settings → Voice.
pub(in crate::gui) const HEADPHONES: &str =
    "Echo cancellation is off: wear headphones, or others hear themselves.";

/// How large a tile is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::gui) enum Size {
    Grid,
    Duo,
    Big,
    Strip,
}

impl Size {
    /// A person's tile: width, height and avatar.
    pub(in crate::gui) fn person(self) -> (f32, f32, f32) {
        match self {
            Size::Grid => (TILE_W, TILE_H, 64.),
            Size::Duo => (DUO_W, DUO_H, 84.),
            Size::Big => (FOCUS_W, FOCUS_H, 112.),
            Size::Strip => (STRIP_W, STRIP_H, 36.),
        }
    }

    /// A stream's tile: as tall as a person's, 16:9.
    pub(in crate::gui) fn stream(self) -> (f32, f32) {
        let (_, h, _) = self.person();
        (h * 16. / 9., h)
    }
}

/// A chip of words: `Push to talk`, `96 kb/s`, `You`.
pub(in crate::gui) fn chip(words: impl Into<SharedString>, color: gpui::Rgba, fill: gpui::Rgba) -> Div {
    div()
        .flex_none()
        .h(px(20.))
        .px(px(6.))
        .rounded(px(theme::R_CHIP))
        .bg(fill)
        .flex()
        .items_center()
        .type_micro()
        .text_color(color)
        .child(words.into())
}

/// How the round button of the call bar looks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::gui) enum BarLook {
    Rest,
    /// My mic or audio is off.
    Off,
    /// The server holds it off: shown, not pressable.
    Held,
    /// Share or camera is on.
    On,
}

/// The round button of the call bar.
pub(in crate::gui) fn bar_button(id: &'static str, icon: &'static str, look: BarLook) -> Stateful<Div> {
    let (fill, color) = match look {
        BarLook::Rest => (theme::selected(), theme::text()),
        BarLook::Off | BarLook::Held => (theme::danger_soft(), theme::danger()),
        BarLook::On => (theme::accent_soft(), theme::accent()),
    };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .relative()
        .size(px(44.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(fill)
        .map(|button| match look {
            BarLook::Held => button.opacity(0.55).aria_disabled(true),
            _ => button
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.bg(theme::pressed()))
                .focus_visible(|style| style.shadow(theme::focus_ring())),
        })
        .child(glyph(icon, 20., color))
}

/// Push to talk, as the pill over the call bar says it: the key to hold, or that it is held.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::gui) enum Talk {
    /// Not push to talk: no pill.
    Open,
    Waiting(Option<String>),
    Talking,
    Priority,
}

pub(in crate::gui) fn talk(push: bool, held: Option<bool>, key: Option<String>) -> Talk {
    match (push, held) {
        (_, Some(true)) => Talk::Priority,
        (true, Some(false)) => Talk::Talking,
        (true, None) => Talk::Waiting(key),
        (false, _) => Talk::Open,
    }
}

/// A round button over a stream: its own click, never the stream's.
fn stream_button(id: &'static str, icon: &'static str, words: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(words)
        .flex_none()
        .size(px(32.))
        .rounded_full()
        .bg(theme::overlay())
        .border_1()
        .border_color(theme::overlay_edge())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .hover(|style| style.bg(theme::selected()))
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .tooltip(tip(words, None))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph(icon, 15., theme::text()))
}

/// The red LIVE mark and whose stream it is, at a stream's foot.
fn caption(words: String, small: bool) -> Div {
    div()
        .absolute()
        .bottom(px(if small { 6. } else { 10. }))
        .left(px(if small { 6. } else { 10. }))
        .max_w(px(if small { 140. } else { 360. }))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(chip("LIVE", theme::on_accent(), theme::danger_fill()).font_weight(theme::SEMIBOLD))
        .when(!small, |row| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .h(px(20.))
                    .px(px(6.))
                    .rounded(px(theme::R_CHIP))
                    .bg(theme::overlay())
                    .flex()
                    .items_center()
                    .type_micro()
                    .text_color(theme::text())
                    .child(words),
            )
        })
}

/// Options that differ between a Tiny room page and a generic main-pane call view.
pub(in crate::gui) struct ViewBar {
    pub held: Option<&'static str>,
    pub held_ears: bool,
    pub ptt_only: bool,
    pub video_allowed: bool,
    pub room_context: bool,
    pub no_sound: bool,
    pub audio_up: bool,
}

impl Shell {
    /// Whether the main pane should draw the standalone call view instead of the conversation.
    pub(in crate::gui) fn call_view_active(&self, snapshot: &Snapshot) -> bool {
        let Some(call) = self.call_shown(snapshot) else {
            return false;
        };
        #[cfg(feature = "gui-shot")]
        if super::shot::view_open() {
            return true;
        }
        self.call_state.view.as_ref() == Some(&call.conversation)
    }

    /// Opens the main-pane call view for `key`.
    pub(in crate::gui) fn call_open_view(&mut self, key: Key, cx: &mut Context<Self>) {
        self.call_state.view = Some(key);
        cx.notify();
    }

    /// Leaves the main-pane call view back to the conversation underneath.
    pub(in crate::gui) fn call_close_view(&mut self, cx: &mut Context<Self>) {
        if self.call_state.view.take().is_some() {
            self.call_state.focus = None;
            cx.notify();
        }
    }

    /// Builds a `CallPeople` lookup for a non-room call.
    fn call_people_for(&self, call: &Call, me_id: &str, me_name: &str, cx: &mut Context<Self>) -> CallPeople {
        let account = call.conversation.account();
        let mut names = HashMap::new();
        let mut pictures = HashMap::new();
        let my_id = if me_id.is_empty() { "me" } else { me_id };
        names.insert(my_id.to_owned(), if me_name.is_empty() { "You".to_owned() } else { me_name.to_owned() });
        for who in &call.participants {
            names.insert(who.id.clone(), who.name.clone());
            if let Some(path) = self.call_participant_picture(&account, who, cx) {
                pictures.insert(who.id.clone(), path);
            }
        }
        CallPeople { conversation: call.conversation.clone(), names, pictures }
    }

    /// My account's id and display name for `call`.
    fn call_me(&self, snapshot: &Snapshot, call: &Call) -> (String, String) {
        let account = call.conversation.account();
        let found = snapshot.accounts.iter().find(|a| a.key == account);
        let id = found.map(|a| a.me.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".to_owned());
        let name = found.map(|a| a.name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "You".to_owned());
        (id, name)
    }

    /// Full main-pane call view for any active call (direct, group, Discord voice, or room viewed
    /// outside its server page).
    pub(in crate::gui) fn call_view_pane(
        &self,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(call) = self.call_shown(snapshot) else {
            return div().size_full();
        };
        let subject = Subject::of(self, snapshot, &call);
        let (me_id, me_name) = self.call_me(snapshot, &call);
        let mut stage = Stage::of_call(&call, &subject, &me_id, &me_name);
        if call.stage {
            super::discord_ux::order_seats(&mut stage.seats, &call.raised, call.request_to_speak);
        }
        let people = self.call_people_for(&call, &me_id, &me_name, cx);

        let phone = crate::gui::phone::active();
        let count = stage.seats.len();
        let count_label = if count == 1 { "1 person".to_owned() } else { format!("{count} people") };
        let place_chip = subject.space.as_ref().map(|space| {
            chip(space.clone(), theme::text_soft(), theme::hover())
        });
        let picker = self.call_add_picker(&call, subject.kind);
        let can_add_people = picker
            || (call.conversation.network == Network::Discord
                && subject.kind.is_direct()
                && call.state == CallState::Joined
                && (call.participants.len() + 1) < 10);

        let back = icon_button("call-view-back", "icons/arrow-left.svg", false)
            .tooltip(tip("Back to conversation (Esc)", None))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.call_close_view(cx)));
        let title_icon = if subject.kind == ConversationKind::Voice {
            "icons/volume-2.svg"
        } else {
            "icons/phone.svg"
        };
        let voice_row = snapshot.open.as_ref().filter(|row| row.key == call.conversation).cloned()
            .or_else(|| self.sidebar_state.find(&self.trees, &call.conversation));
        let voice_status = voice_row.as_ref().and_then(crate::gui::sidebar::discord_voice_status);
        let voice_details = call.conversation.network == Network::Discord && call.state == CallState::Joined
            && subject.kind == ConversationKind::Voice;
        let details_key = call.conversation.clone();
        let title_row = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(back)
            .child(glyph(
                title_icon,
                16.,
                if call.state == CallState::Joined { theme::positive() } else { theme::text_soft() },
            ))
            .child(
                div()
                    .flex_none()
                    .max_w(px(260.))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .type_title()
                            .text_color(theme::text())
                            .child(subject.title.clone()),
                    )
                    .children((!call.stage_topic.is_empty()).then(|| {
                        div().truncate().type_micro().text_color(theme::text_soft()).child(call.stage_topic.clone())
                    }))
                    .children(call.activity.as_ref().map(|activity| {
                        div().truncate().type_micro().text_color(theme::text_soft()).child(super::discord_ux::activity_line(&activity.name))
                    })),
            )
            .children(place_chip)
            .children(voice_status.map(|status| self.voice_status_line(&call.conversation, &status, cx)))
            .child(chip(count_label, theme::text_soft(), theme::hover()))
            .children(self.call_security(&call, Place::TopBar, cx));
        let add_people = can_add_people.then(|| {
            button_with_icon(
                "call-view-add-people",
                ButtonKind::Secondary,
                "icons/user-plus.svg",
                "Add people",
            )
            .h(px(28.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener({
                let picker = picker;
                move |this, _, window, cx| {
                    if picker {
                        this.open_call_people(window, cx);
                    } else {
                        this.call_menu_toggle(Menu::AddPeople, cx);
                    }
                }
            }))
        });
        let bar = drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .pl(px(theme::PANE_PAD))
            .pr(px(chrome::end_pad(End::Conversation, 12.)))
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .when(!phone, |bar| bar.child(self.strip_controls(window, cx)))
            .child(title_row)
            .children(voice_details.then(|| icon_button("call-voice-region", "icons/info.svg", false)
                .tooltip(tip("Connection details", None))
                .on_click(cx.listener(move |this, _, _, cx| this.open_discord_voice_settings(details_key.clone(), false, cx)))))
            .children(add_people);

        let held = if call.server_muted {
            Some("A moderator muted you here")
        } else if call.suppressed {
            Some("You can't speak in this channel")
        } else {
            None
        };
        let banner = self.call_view_banner(&subject.title, &call, cx);
        let stage_el = self.call_stage(&stage, &people, Some(&call), false, cx);
        let controls = self.call_view_bar(
            Some(&call),
            &ViewBar {
                held,
                held_ears: call.server_deafened,
                ptt_only: false,
                video_allowed: stage.video,
                room_context: false,
                no_sound: false,
                audio_up: true,
            },
            cx,
        );
        div().size_full().child(
            div()
                .id("call-view-pane")
                .size_full()
                .flex()
                .flex_col()
                .child(bar)
                .children(banner)
                .children(self.call_discord_face(&call, &stage.seats, cx))
                .child(stage_el)
                .child(controls),
        )
    }

    /// Over the stage while my call here is not simply up: connecting or reconnecting with how
    /// long it has been, or what ended it, with Rejoin.
    pub(in crate::gui) fn call_view_banner(
        &self,
        title_place: &str,
        call: &Call,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (icon, color, fill, title, line): (
            &'static str,
            gpui::Rgba,
            gpui::Rgba,
            String,
            Option<String>,
        ) = match &call.state {
            CallState::RingingOut => (
                "icons/phone-outgoing.svg",
                theme::accent_text(),
                theme::accent_wash(),
                format!("Calling {title_place}…"),
                None,
            ),
            CallState::Connecting => (
                "icons/refresh-cw.svg",
                theme::accent_text(),
                theme::accent_wash(),
                format!("Connecting to {title_place}…"),
                None,
            ),
            CallState::Reconnecting => (
                "icons/wifi-off.svg",
                theme::warning(),
                theme::warning_soft(),
                "Connection lost. Reconnecting…".to_owned(),
                Some("Your seat is kept while the call finds its way back.".to_owned()),
            ),
            CallState::Failed { .. } => {
                let (title, line, _) = self.call_failure(call)?;
                ("icons/triangle-alert.svg", theme::danger(), theme::danger_soft(), title, Some(line))
            }
            _ => return None,
        };
        let failed = matches!(call.state, CallState::Failed { .. });
        let again = self.call_failure(call).filter(|(_, _, again)| *again).is_some();
        let key = call.conversation.clone();
        let action: Option<AnyElement> = if failed {
            again.then(|| {
                button_with_icon("tiny-banner-rejoin", ButtonKind::Primary, "icons/rotate-ccw.svg", "Rejoin")
                    .h(px(30.))
                    .on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
                    .into_any_element()
            })
        } else {
            let word = if matches!(call.state, CallState::Connecting | CallState::RingingOut) {
                "Cancel"
            } else {
                "Leave"
            };
            Some(
                button_with_icon("tiny-banner-leave", ButtonKind::Danger, "icons/phone-off.svg", word)
                    .h(px(30.))
                    .tooltip(tip(word, Some("⌘⇧H")))
                    .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)))
                    .into_any_element(),
            )
        };
        let dismiss = failed.then(|| {
            div()
                .id("tiny-banner-dismiss")
                .flex_none()
                .size(px(28.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.bg(theme::hover()))
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .tooltip(tip("Dismiss", Some("⌘⇧H")))
                .on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)))
                .child(glyph("icons/x.svg", 14., theme::text_soft()))
        });
        let clock = self.call_stage_clock(call, cx);
        Some(
            div().flex_none().px(px(24.)).pt(px(16.)).flex().justify_center().child(
                div()
                    .id("tiny-call-banner")
                    .w_full()
                    .max_w(px(640.))
                    .min_h(px(52.))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(theme::R_FLOAT))
                    .bg(fill)
                    .border_1()
                    .border_color(theme::hairline())
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(30.))
                            .rounded_full()
                            .bg(theme::hover())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph(icon, 15., color)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .min_w(px(120.))
                                            .flex_1()
                                            .whitespace_normal()
                                            .line_clamp(2)
                                            .type_ui()
                                            .font_weight(theme::SEMIBOLD)
                                            .text_color(theme::text())
                                            .child(title),
                                    )
                                    .children(clock.map(|clock| {
                                        div().flex_none().type_small().text_color(color).child(clock)
                                    })),
                            )
                            .children(line.map(|line| {
                                div()
                                    .whitespace_normal()
                                    .type_small()
                                    .text_color(theme::text_soft())
                                    .child(line)
                            })),
                    )
                    .children(action)
                    .children(dismiss),
            ),
        )
    }

    /// The stage: wrapping and centred tiles (or two large tiles for a 1:1 call, or peer video
    /// large with self PiP); or one person large over a row of the rest; or one stream filling the
    /// stage.
    pub(in crate::gui) fn call_stage(
        &self,
        stage: &Stage,
        people: &dyn People,
        call: Option<&Call>,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let me = stage.me_id().unwrap_or("");
        let streams = stage::streams(stage, me, sending_now(call));
        #[cfg(feature = "gui-shot")]
        let shot_focus = super::shot::focus();
        #[cfg(not(feature = "gui-shot"))]
        let shot_focus: Option<Focus> = None;
        let active_focus = shot_focus.as_ref().or(self.call_state.focus.as_ref());
        let focus = stage::still(active_focus, stage, &streams);
        let base = div().id("tiny-stage").flex_1().min_h_0();
        match focus {
            None => {
                // Direct 1:1 video call: if the peer has a live frame on screen, draw it large
                // with my own camera/tile in the corner.
                if stage.kind == ConversationKind::Direct && stage.seats.len() <= 2 {
                    if let Some(peer_stream) = streams.iter().find(|s| {
                        let kind = if s.share { PictureKind::Share } else { PictureKind::Camera };
                        !s.mine && self.call_frame(call, &s.user_id, kind).is_some()
                    }) {
                        let main = self.call_stream_view(stage, people, peer_stream, call, false, cx);
                        let self_frame = self
                            .call_self_frame(call, false)
                            .or_else(|| self.call_self_frame(call, true));
                        let pip = self_frame.map(|frame| {
                            div()
                                .absolute()
                                .bottom(px(16.))
                                .right(px(16.))
                                .w(px(184.))
                                .h(px(108.))
                                .rounded(px(10.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(theme::border())
                                .bg(gpui::rgb(0x0b0b0d))
                                .shadow(theme::shadow_popover())
                                .child(img(frame).size_full().object_fit(ObjectFit::Contain))
                        });
                        return base
                            .p(px(16.))
                            .flex()
                            .child(div().relative().flex_1().min_h_0().flex().child(main).children(pip))
                            .into_any_element();
                    }
                }
                let size = if stage.seats.len() <= 2 && streams.is_empty() {
                    Size::Duo
                } else {
                    Size::Grid
                };
                let (grid_seats, roster_seats) =
                    stage::partition_stage(&stage.seats, &streams, me, stage::STAGE_GRID_CAP);
                let grid_tiles = self.call_tiles_for(
                    stage,
                    people,
                    &grid_seats,
                    &streams,
                    call,
                    None,
                    size,
                    room_context,
                    cx,
                );
                let mut content = base
                    .overflow_y_scroll()
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(20.))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .content_center()
                            .items_center()
                            .justify_center()
                            .gap(px(12.))
                            .children(grid_tiles),
                    );
                if !roster_seats.is_empty() {
                    let mut roster = div()
                        .w_full()
                        .max_w(px(680.))
                        .p(px(12.))
                        .rounded(px(theme::R_CARD))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .px(px(6.))
                                .pb(px(4.))
                                .type_micro()
                                .text_color(theme::text_faint())
                                .child(format!("ALSO IN CALL · {}", roster_seats.len())),
                        );
                    let account = stage.key.account();
                    for seat in roster_seats {
                        let name = people.name(&seat.id);
                        let avatar = Avatar::new(&people.key(&seat.id), &name, 20.)
                            .image_at(people.picture(&seat.id))
                            .build();
                        let glyphs = seat.glyphs();
                        let menu_id = seat.id.clone();
                        let menu_name = name.clone();
                        let menu_account = account.clone();
                        roster = roster.child(
                            div()
                                .id(SharedString::from(format!("tiny-roster-{}", seat.id)))
                                .h(px(28.))
                                .px(px(6.))
                                .rounded(px(theme::R_CONTROL))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .hover(|s| s.bg(theme::hover()))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        if room_context {
                                            this.tiny_open_float(
                                                Float::Person {
                                                    user_id: menu_id.clone(),
                                                    at: event.position,
                                                },
                                                cx,
                                            );
                                        } else {
                                            this.call_menu_set(
                                                Some(Menu::Person {
                                                    account: menu_account.clone(),
                                                    user_id: menu_id.clone(),
                                                    name: menu_name.clone(),
                                                }),
                                                cx,
                                            );
                                        }
                                    }),
                                )
                                .child(avatar)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .type_ui()
                                        .text_color(theme::text_soft())
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .children(
                                            glyphs
                                                .into_iter()
                                                .map(|(icon, color, _)| glyph(icon, 14., color)),
                                        )
                                        .children(call.and_then(|call| call.effect.as_ref()).filter(|effect| {
                                            effect.user_id == seat.id && super::discord_ux::effect_visible(effect.at, super::now())
                                        }).map(|effect| div().type_small().child(super::discord_ux::effect_label(&effect.emoji)))),
                                ),
                        );
                    }
                    content = content.child(roster);
                }
                crate::gui::components::scroll::scroll_page("tiny-stage-grid-scroll-frame", content)
                    .into_any_element()
            }
            Some(Focus::Person(focus)) => {
                let big = stage
                    .seats
                    .iter()
                    .find(|seat| seat.id == *focus)
                    .map(|seat| self.call_tile(stage, people, seat, call, Size::Big, room_context, cx));
                let rest = self.call_tiles(
                    stage,
                    people,
                    &streams,
                    call,
                    Some(&Focus::Person(focus.clone())),
                    Size::Grid,
                    room_context,
                    cx,
                );
                crate::gui::components::scroll::scroll_page(
                    "tiny-stage-person-scroll-frame",
                    base.overflow_y_scroll()
                        .p(px(24.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(16.))
                        .children(big)
                        .child(div().flex().flex_wrap().justify_center().gap(px(12.)).children(rest)),
                )
                .into_any_element()
            }
            Some(Focus::Stream(user_id)) => {
                let Some(stream) = streams.iter().find(|stream| stream.user_id == *user_id) else {
                    return base.into_any_element();
                };
                let strip = self.call_state.strip;
                let view = self.call_stream_view(stage, people, stream, call, false, cx);
                let row = div()
                    .id("tiny-strip")
                    .max_w_full()
                    .overflow_x_scroll()
                    .flex()
                    .gap(px(8.))
                    .children(self.call_tiles(
                        stage,
                        people,
                        &streams,
                        call,
                        Some(&Focus::Stream(user_id.clone())),
                        Size::Strip,
                        room_context,
                        cx,
                    ));
                let base = base.p(px(16.)).flex().flex_col().gap(px(12.));
                if strip.hidden {
                    base.child(view).into_any_element()
                } else if strip.over {
                    base.child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .child(view)
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(12.))
                                    .left_0()
                                    .right_0()
                                    .px(px(12.))
                                    .flex()
                                    .justify_center()
                                    .child(row.p(px(6.)).rounded(px(16.)).bg(theme::overlay())),
                            ),
                    )
                    .into_any_element()
                } else {
                    base.child(view)
                        .child(div().flex_none().flex().justify_center().child(row))
                        .into_any_element()
                }
            }
        }
    }

    /// Everyone's tile, each followed by their stream's, leaving out the one focused.
    #[allow(clippy::too_many_arguments)]
    fn call_tiles(
        &self,
        stage: &Stage,
        people: &dyn People,
        streams: &[Stream],
        call: Option<&Call>,
        except: Option<&Focus>,
        size: Size,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let seats: Vec<&Seat> = stage.seats.iter().collect();
        self.call_tiles_for(stage, people, &seats, streams, call, except, size, room_context, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn call_tiles_for(
        &self,
        stage: &Stage,
        people: &dyn People,
        seats: &[&Seat],
        streams: &[Stream],
        call: Option<&Call>,
        except: Option<&Focus>,
        size: Size,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let shown = |focus: Focus| except != Some(&focus);
        let mut tiles = Vec::new();
        for seat in seats {
            if shown(Focus::Person(seat.id.clone())) {
                tiles.push(self.call_tile(stage, people, seat, call, size, room_context, cx));
            }
            if let Some(stream) = streams
                .iter()
                .find(|stream| stream.user_id == seat.id)
                .filter(|stream| shown(Focus::Stream(stream.user_id.clone())))
            {
                tiles.push(self.call_stream_tile(stage, people, stream, call, size, cx));
            }
        }
        // My stream before the stage lists my seat.
        for stream in streams.iter().filter(|stream| !seats.iter().any(|seat| seat.id == stream.user_id)) {
            if shown(Focus::Stream(stream.user_id.clone())) {
                tiles.push(self.call_stream_tile(stage, people, stream, call, size, cx));
            }
        }
        tiles
    }

    /// What a stream shows: its picture; mine, the same small live view of it that I send, with
    /// Stop over it; anyone else's, "Watch stream" until its first picture arrives.
    fn call_stream_body(
        &self,
        people: &dyn People,
        stream: &Stream,
        call: Option<&Call>,
        size: Size,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = if stream.share { PictureKind::Share } else { PictureKind::Camera };
        if let Some(frame) = self.call_frame(call, &stream.user_id, kind) {
            return img(frame).size_full().object_fit(ObjectFit::Contain).into_any_element();
        }
        let small = size == Size::Strip;
        let large = size == Size::Big;
        let body = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(if small { 4. } else { 10. }))
            .p(px(8.));
        if stream.mine {
            if let Some(frame) = self.call_self_frame(call, stream.share) {
                let stop = if stream.share {
                    button_with_icon("tiny-stream-stop", ButtonKind::Danger, "icons/monitor-off.svg", "Stop sharing")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.tiny_stop_share(cx);
                        }))
                } else {
                    button_with_icon("tiny-stream-camera-off", ButtonKind::Danger, "icons/video-off.svg", "Turn camera off")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_ask(Request::Media(MediaRequest::Camera { on: false }), cx);
                        }))
                };
                return div()
                    .size_full()
                    .relative()
                    .child(img(frame).size_full().object_fit(ObjectFit::Contain))
                    .when(!small, |tile| {
                        let row = div().absolute().left_0().right_0().flex().justify_center();
                        let row = if matches!(size, Size::Grid | Size::Duo) {
                            row.top(px(8.))
                        } else {
                            row.bottom(px(10.))
                        };
                        tile.child(
                            row.child(
                                div()
                                    .p(px(2.))
                                    .rounded(px(10.))
                                    .bg(theme::overlay())
                                    .child(stop.h(px(28.)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())),
                            ),
                        )
                    })
                    .into_any_element();
            }
        }
        if stream.mine {
            let words = if stream.share { "You're sharing your screen" } else { "Your camera is on" };
            let stop = if stream.share {
                button_with_icon("tiny-stream-stop", ButtonKind::Danger, "icons/monitor-off.svg", "Stop sharing")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.tiny_stop_share(cx);
                    }))
            } else {
                button_with_icon("tiny-stream-camera-off", ButtonKind::Danger, "icons/video-off.svg", "Turn camera off")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_ask(Request::Media(MediaRequest::Camera { on: false }), cx);
                    }))
            };
            let line = match &self.tiny_state.shared {
                Some(screen) if stream.share => format!(
                    "Everyone in the call can watch {}. A live view of it shows here as soon as the first picture is sent.",
                    screen.name
                ),
                _ => "Everyone in the call can watch it.".to_owned(),
            };
            return body
                .when(small || large, |body| {
                    body.child(glyph(
                        if stream.share { "icons/monitor-up.svg" } else { "icons/video.svg" },
                        if small { 18. } else { 36. },
                        theme::accent_text(),
                    ))
                })
                .when(!small, |body| {
                    body.child(
                        div()
                            .text_center()
                            .whitespace_normal()
                            .type_ui()
                            .font_weight(theme::SEMIBOLD)
                            .text_color(theme::text())
                            .child(words),
                    )
                })
                .when(large, |body| {
                    body.child(
                        div()
                            .max_w(px(420.))
                            .text_center()
                            .whitespace_normal()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(line),
                    )
                })
                .when(!small, |body| {
                    body.child(stop.h(px(28.)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
                })
                .into_any_element();
        }
        let name = people.name(&stream.user_id);
        if !stream.watchable {
            return body
                .when(!small, |body| {
                    body.child(
                        Avatar::new(&people.key(&stream.user_id), &name, if large { 72. } else { 40. })
                            .image_at(people.picture(&stream.user_id))
                            .build(),
                    )
                })
                .child(div().type_small().text_color(theme::text_soft()).child("Camera on"))
                .into_any_element();
        }
        let asked = call.is_some_and(|call| watching(call, &stream.user_id));
        let since = self
            .call_state
            .watch_asked
            .as_ref()
            .filter(|(id, _)| *id == stream.user_id)
            .map(|(_, at)| at.elapsed())
            .unwrap_or_default();
        let trouble = asked
            .then(|| call.and_then(|c| stage::watch_trouble(c, &stream.user_id, |id| people.name(id), since)))
            .flatten();
        if let Some(trouble) = trouble {
            let words = match &trouble {
                stage::Trouble::OtherSource { name: who, kind } => {
                    let what = if *kind == PictureKind::Share { "screen" } else { "camera" };
                    format!("The room is showing {who}'s {what} right now.")
                }
                stage::Trouble::Deafened => "Undeafen to watch.".to_owned(),
                stage::Trouble::Slow => format!("Waiting for {name}'s picture… Your connection may be too slow for video."),
                stage::Trouble::Failed => format!("Couldn't get {name}'s picture."),
                stage::Trouble::BudgetFull => "Too many videos".to_owned(),
            };
            let retry_id = stream.user_id.clone();
            let retry = matches!(trouble, stage::Trouble::Slow | stage::Trouble::Failed).then(|| {
                div()
                    .id(SharedString::from(format!("tiny-stream-retry-{}", stream.user_id)))
                    .h(px(if small { 22. } else { 28. }))
                    .px(px(if small { 8. } else { 12. }))
                    .rounded_full()
                    .bg(theme::accent())
                    .text_color(theme::on_accent())
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .type_micro()
                    .font_weight(theme::SEMIBOLD)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_retry_watch(retry_id.clone(), cx);
                    }))
                    .child("Try again")
            });
            return body
                .when(!small, |body| {
                    body.child(
                        Avatar::new(&people.key(&stream.user_id), &name, if large { 72. } else { 40. })
                            .image_at(people.picture(&stream.user_id))
                            .build(),
                    )
                })
                .child(
                    div()
                        .max_w(px(260.))
                        .text_center()
                        .whitespace_normal()
                        .type_small()
                        .text_color(theme::text_soft())
                        .child(words),
                )
                .children(retry)
                .into_any_element();
        }
        let pill = div()
            .h(px(if small { 22. } else { 30. }))
            .px(px(if small { 8. } else { 14. }))
            .rounded_full()
            .bg(if asked { theme::overlay() } else { theme::accent() })
            .flex()
            .items_center()
            .gap(px(6.))
            .when(!asked && !small, |pill| pill.child(glyph("icons/play.svg", 12., theme::on_accent())))
            .child(
                div()
                    .map(|words| if small { words.type_micro() } else { words.type_small() })
                    .font_weight(theme::SEMIBOLD)
                    .text_color(if asked { theme::text_soft() } else { theme::on_accent() })
                    .child(if asked { "Connecting…" } else { "Watch stream" }),
            );
        body.when(!small, |body| {
            body.child(
                Avatar::new(&people.key(&stream.user_id), &name, if large { 72. } else { 40. })
                    .image_at(people.picture(&stream.user_id))
                    .build(),
            )
        })
        .child(pill)
        .into_any_element()
    }

    /// A stream in the grid or the strip: click it to watch it filling the stage.
    fn call_stream_tile(
        &self,
        _stage: &Stage,
        people: &dyn People,
        stream: &Stream,
        call: Option<&Call>,
        size: Size,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (w, h) = size.stream();
        let small = size == Size::Strip;
        let name = people.name(&stream.user_id);
        let what = if stream.share { "screen" } else { "camera" };
        let kind = if stream.share { PictureKind::Share } else { PictureKind::Camera };
        let live = self.call_frame(call, &stream.user_id, kind).is_some();
        let user_id = stream.user_id.clone();
        let mine = stream.mine;
        div()
            .id(SharedString::from(format!("tiny-stream-{}", stream.user_id)))
            .relative()
            .flex_none()
            .w(px(w))
            .h(px(h))
            .rounded(px(theme::R_CARD + 4.))
            .bg(gpui::rgb(0x0b0b0d))
            .border_2()
            .border_color(if live { theme::border_strong() } else { theme::hairline() })
            .overflow_hidden()
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .tooltip(tip(
                if mine || live { "Click to focus".to_owned() } else { format!("Watch {name}'s {what}") },
                None,
            ))
            .on_click(cx.listener(move |this, _, window, cx| this.call_open_stream(&user_id, mine, window, cx)))
            .child(self.call_stream_body(people, stream, call, size, cx))
            .child(caption(
                if mine { format!("Your {what}") } else { format!("{name}'s {what}") },
                small,
            ))
            .into_any_element()
    }

    /// The focused stream, filling the stage (or, `full`, the window).
    fn call_stream_view(
        &self,
        _stage: &Stage,
        people: &dyn People,
        stream: &Stream,
        call: Option<&Call>,
        full: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let strip = self.call_state.strip;
        let name = people.name(&stream.user_id);
        let what = if stream.share { "screen" } else { "camera" };
        let user_id = stream.user_id.clone();
        let mine = stream.mine;
        let mut tools = div().absolute().top(px(10.)).right(px(10.)).flex().gap(px(6.));
        if full {
            tools = tools.child(
                stream_button("tiny-stream-unfull", "icons/minimize.svg", "Exit full screen (Esc)").on_click(
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.call_unfull(window, cx);
                    }),
                ),
            );
        } else {
            tools = tools
                .child(
                    stream_button(
                        "tiny-stream-people",
                        "icons/users.svg",
                        if strip.hidden { "Show participants" } else { "Hide participants" },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.call_set_strip(stage::Strip { hidden: !strip.hidden, ..strip }, window, cx);
                    })),
                )
                .when(!strip.hidden, |tools| {
                    tools.child(
                        stream_button(
                            "tiny-stream-over",
                            "icons/panel-bottom.svg",
                            if strip.over { "Participants under the stream" } else { "Participants over the stream" },
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.call_set_strip(stage::Strip { over: !strip.over, ..strip }, window, cx);
                        })),
                    )
                })
                .child(
                    stream_button(
                        "tiny-stream-popout",
                        "icons/picture-in-picture-2.svg",
                        if self.call_popout_open() { "Bring back from pop-out" } else { "Pop out" },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_toggle_popout(cx);
                    })),
                )
                .child(stream_button("tiny-stream-full", "icons/maximize.svg", "Full screen").on_click(
                    cx.listener(|this, _, window: &mut Window, cx| {
                        cx.stop_propagation();
                        this.call_full(window, cx);
                    }),
                ));
        }
        if !mine {
            tools = tools.child(stream_button("tiny-stream-unwatch", "icons/x.svg", "Stop watching").on_click(
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.call_unfull(window, cx);
                    this.call_stop_watching(window, cx);
                }),
            ));
        }
        // The pop-out window has the frame: the stage never draws the same picture twice.
        let body: AnyElement = if self.call_popout_open() {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .type_ui()
                        .text_color(theme::text_soft())
                        .child(glyph("icons/picture-in-picture-2.svg", 16., theme::accent()))
                        .child("Playing in a separate window"),
                )
                .child(
                    button("tiny-stream-bring-back", ButtonKind::Secondary, "Bring back")
                        .h(px(30.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_close_popout(cx);
                        })),
                )
                .into_any_element()
        } else {
            self.call_stream_body(people, stream, call, Size::Big, cx).into_any_element()
        };
        let volume = call
            .filter(|_| stream.share && !mine)
            .and_then(|call| self.call_stream_volume(call, &user_id, cx))
            .map(|controls| controls.absolute().top(px(10.)).left(px(10.)));
        div()
            .id(if full { "tiny-stream-whole" } else { "tiny-stream-focused" })
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .when(full, |view| view.size_full())
            .when(!full, |view| view.rounded(px(12.)).border_1().border_color(theme::hairline()))
            .bg(gpui::rgb(0x000000))
            .overflow_hidden()
            .when(!full, |view| {
                view.cursor_pointer()
                    .tooltip(tip("Click to see everyone again (Esc)", None))
                    .on_click(cx.listener(move |this, _, window, cx| this.call_open_stream(&user_id, mine, window, cx)))
            })
            .child(body)
            .children(volume)
            .child(caption(
                if mine { format!("Your {what}") } else { format!("{name}'s {what}") },
                false,
            ))
            .child(tools)
    }

    /// Over the whole window while a focused stream has full screen — works for any call, with or
    /// without a Tiny server page loaded.
    pub(in crate::gui) fn call_full_stage(
        &self,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.call_state.full?;
        // Prefer the loaded Tiny room page if it is showing the focused room, else use the active call.
        if let Some(page) = self.tiny_page_facts() {
            if let Some(room) = page
                .community
                .rooms
                .iter()
                .find(|room| Some(&room.id) == self.tiny_state.channel.as_ref())
            {
                let call = self.tiny_call().filter(|call| call.conversation.id == room.id);
                let stage = Stage::of_room(&page, room, call.as_ref());
                return Some(self.call_full_stage_inner(&stage, &page, call.as_ref(), cx));
            }
        }
        let call = self.call_shown(snapshot)?;
        let subject = Subject::of(self, snapshot, &call);
        let (me_id, me_name) = self.call_me(snapshot, &call);
        let stage = Stage::of_call(&call, &subject, &me_id, &me_name);
        let people = self.call_people_for(&call, &me_id, &me_name, cx);
        Some(self.call_full_stage_inner(&stage, &people, Some(&call), cx))
    }

    fn call_full_stage_inner(
        &self,
        stage: &Stage,
        people: &dyn People,
        call: Option<&Call>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let me = stage.me_id().unwrap_or("");
        let streams = stage::streams(stage, me, sending_now(call));
        let stream = match stage::still(self.call_state.focus.as_ref(), stage, &streams) {
            Some(Focus::Stream(user_id)) => streams.iter().find(|stream| stream.user_id == *user_id),
            _ => None,
        };
        let inside: AnyElement = match stream {
            Some(stream) => self.call_stream_view(stage, people, stream, call, true, cx).into_any_element(),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .child(div().type_title().text_color(theme::text()).child("The stream ended"))
                .child(
                    button_with_icon(
                        "tiny-stream-ended",
                        ButtonKind::Secondary,
                        "icons/minimize.svg",
                        "Exit full screen",
                    )
                    .h(px(32.))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.call_unfull(window, cx);
                    })),
                )
                .into_any_element(),
        };
        div()
            .id("tiny-full")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(gpui::rgb(0x000000))
            .occlude()
            .flex()
            .child(inside)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn call_tile(
        &self,
        stage: &Stage,
        people: &dyn People,
        seat: &Seat,
        call: Option<&Call>,
        size: Size,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let big = size == Size::Big;
        let small = size == Size::Strip;
        let me = seat.me;
        let name = if seat.name.is_empty() { people.name(&seat.id) } else { seat.name.clone() };
        let speaking = seat.speaking && !seat.server_muted && !seat.suppressed;
        let color = people.color(&seat.id).map(gpui::rgb).unwrap_or(theme::text());
        let states = seat.states();
        let mut glyphs = seat.glyphs();
        let account = stage.key.account();
        let (percent, local_mute) = self.call_gain(&account, &seat.id);
        if local_mute && !me {
            glyphs.push(("icons/volume-x.svg", theme::warning(), "Muted for you"));
        } else if percent != 100 && !me {
            glyphs.push(("icons/volume-2.svg", theme::text_soft(), "Volume adjusted"));
        }
        let focused = self.call_state.focus.as_ref() == Some(&Focus::Person(seat.id.clone()));
        let group: SharedString = format!("tiny-tile-{}", seat.id).into();
        let (w, h, avatar) = size.person();
        let user_id = seat.id.clone();
        let focus_id = seat.id.clone();
        let menu_id = seat.id.clone();
        let menu_name = name.clone();
        let menu_account = account.clone();
        let right_name = name.clone();
        let right_account = account;
        // My own link, in my own tile: what everyone else hears me through.
        let link = (me && !small && call.is_some_and(|call| call.state == CallState::Joined))
            .then(|| self.tiny_health())
            .flatten()
            .map(|health| {
                let grade = health.grade();
                let words = match health.rtt_ms {
                    Some(ms) if grade != Grade::Unknown => format!("{} · {ms} ms", grade.words()),
                    _ => grade.words().to_owned(),
                };
                div()
                    .id("tiny-tile-link")
                    .absolute()
                    .top(px(10.))
                    .left(px(10.))
                    .p(px(3.))
                    .rounded(px(theme::R_CHIP))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .tooltip(tip(format!("{words}. Call health"), None))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.tiny_toggle_health(cx);
                    }))
                    .child(health::bars(grade, 11.))
            });
        let hover_button = |id: String, icon: &'static str, words: &'static str| {
            div()
                .id(SharedString::from(id))
                .size(px(28.))
                .rounded_full()
                .bg(theme::overlay())
                .border_1()
                .border_color(theme::overlay_edge())
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme::selected()))
                .tooltip(tip(words, None))
                .child(glyph(icon, 14., theme::text()))
        };
        let actions = div()
            .absolute()
            .bottom(px(8.))
            .right(px(8.))
            .flex()
            .gap(px(4.))
            .opacity(if focused { 1. } else { 0. })
            .group_hover(group.clone(), |style| style.opacity(1.))
            .when(!me, |row| {
                row.child(
                    hover_button(
                        format!("tiny-tile-volume-{menu_id}"),
                        if local_mute { "icons/volume-x.svg" } else { "icons/volume-2.svg" },
                        "Volume and more",
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if room_context {
                                this.tiny_open_float(
                                    Float::Person { user_id: menu_id.clone(), at: event.position },
                                    cx,
                                );
                            } else {
                                this.call_menu_set(
                                    Some(Menu::Person {
                                        account: menu_account.clone(),
                                        user_id: menu_id.clone(),
                                        name: menu_name.clone(),
                                    }),
                                    cx,
                                );
                            }
                        }),
                    ),
                )
            })
            .child(hover_button(
                format!("tiny-tile-pin-{}", seat.id),
                if focused { "icons/x.svg" } else { "icons/pin.svg" },
                if focused { "Stop focusing" } else { "Focus" },
            ));
        let state_row = div()
            .h(px(20.))
            .flex()
            .items_center()
            .gap(px(4.))
            .children(states.first().map(|words| {
                let danger = words.starts_with("Server") || words.starts_with("Can't");
                let priority = *words == "Priority speaker";
                let (fg, bg) = if danger {
                    (theme::danger(), theme::danger_soft())
                } else if priority {
                    (theme::accent_text(), theme::accent_soft())
                } else {
                    (theme::text_soft(), theme::hover())
                };
                chip(*words, fg, bg)
            }))
            .when(local_mute && !me, |row| row.child(chip("Muted for you", theme::warning(), theme::warning_soft())))
            .children(call.and_then(|call| call.effect.as_ref()).filter(|effect| {
                effect.user_id == seat.id && super::discord_ux::effect_visible(effect.at, super::now())
            }).map(|effect| chip(super::discord_ux::effect_label(&effect.emoji), theme::accent_text(), theme::accent_soft())))
            .when(percent != 100 && !local_mute && !me, |row| {
                row.child(chip(format!("{percent} %"), theme::text_soft(), theme::hover()))
            });
        let ring = if speaking && seat.priority {
            theme::accent_text()
        } else if speaking {
            theme::positive()
        } else if focused {
            theme::border_strong()
        } else {
            theme::hairline()
        };
        let camera_frame = seat.camera.then(|| {
            if me {
                self.call_self_frame(call, false)
            } else {
                self.call_frame(call, &seat.id, PictureKind::Camera)
            }
        }).flatten();
        let camera_refused = !me
            && seat.camera
            && camera_frame.is_none()
            && call.is_some_and(|c| {
                c.watch_refused
                    .iter()
                    .any(|(uid, kind)| uid == &seat.id && *kind == PictureKind::Camera)
            });
        let base_tile = div()
            .id(SharedString::from(format!("tiny-tile-{}", seat.id)))
            .group(group)
            .relative()
            .flex_none()
            .w(px(w))
            .h(px(h))
            .rounded(px(theme::R_CARD + 4.))
            .bg(if camera_frame.is_some() { gpui::rgb(0x0b0b0d) } else { theme::raised() })
            .overflow_hidden()
            .border_2()
            .border_color(ring)
            .when(speaking, |tile| {
                tile.shadow(vec![gpui::BoxShadow {
                    color: gpui::Rgba {
                        alpha: 0.25,
                        ..if seat.priority { theme::accent_text() } else { theme::positive() }
                    }
                    .into(),
                    offset: gpui::point(px(0.), px(0.)),
                    blur_radius: px(12.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(if small { 4. } else { 8. }))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .tooltip(tip(
                if focused { "Click to stop focusing" } else { "Click to focus. Right-click for more" },
                None,
            ))
            .on_click(cx.listener(move |this, _, window, cx| this.call_toggle_focus(&focus_id, window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if room_context {
                        this.tiny_open_float(Float::Person { user_id: user_id.clone(), at: event.position }, cx);
                    } else if !me {
                        this.call_menu_set(
                            Some(Menu::Person {
                                account: right_account.clone(),
                                user_id: user_id.clone(),
                                name: right_name.clone(),
                            }),
                            cx,
                        );
                    }
                }),
            );
        let tile = if let Some(frame) = camera_frame {
            base_tile
                .child(img(frame).size_full().object_fit(ObjectFit::Cover))
                .child(caption(if me { format!("{name} (You)") } else { name }, small))
        } else {
            base_tile
                .child(Avatar::new(&people.key(&seat.id), &name, avatar).image_at(people.picture(&seat.id)).build())
                .child(
                    div()
                        .max_w(px(w - 24.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .map(|name| {
                                    if big {
                                        name.type_title()
                                    } else if small {
                                        name.type_small()
                                    } else {
                                        name.type_ui()
                                    }
                                })
                                .font_weight(theme::SEMIBOLD)
                                .text_color(color)
                                .child(name),
                        )
                        .when(me && !small, |row| row.child(chip("You", theme::text_soft(), theme::hover()))),
                )
                .when(camera_refused, |tile| {
                    tile.child(chip("Too many videos", theme::text_soft(), theme::hover()))
                })
                .when(!small, |tile| tile.child(state_row))
        };
        tile.children(link)
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .flex()
                    .gap(px(4.))
                    .children(glyphs.into_iter().map(|(icon, color, _)| glyph(icon, 14., color))),
            )
            .when(!small, |tile| tile.child(actions))
            .into_any_element()
    }

    /// Over the call bar: "You're muted", moderator mute, push-to-talk key, and serious health hint.
    pub(in crate::gui) fn call_view_pills(
        &self,
        ptt_only: bool,
        call: Option<&Call>,
        held_by_server: Option<&'static str>,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let call = call.filter(|call| call.state == CallState::Joined)?;
        let pill = |id: &'static str, icon: &'static str, words: String, color: gpui::Rgba, fill: gpui::Rgba| {
            div()
                .id(id)
                .min_h(px(30.))
                .max_w_full()
                .px(px(12.))
                .py(px(4.))
                .rounded(px(15.))
                .bg(fill)
                .border_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .gap(px(7.))
                .type_small()
                .font_weight(theme::MEDIUM)
                .text_color(color)
                .child(div().flex_none().child(glyph(icon, 14., color)))
                .child(div().min_w(px(80.)).whitespace_normal().line_clamp(3).child(words))
        };
        let muted = match held_by_server {
            Some(words) => Some(pill("tiny-pill-held", "icons/mic-off.svg", words.to_owned(), theme::danger(), theme::danger_soft()).into_any_element()),
            None if self.call_muted_talking(call) => Some(
                pill(
                    "tiny-pill-muted",
                    "icons/mic-off.svg",
                    if crate::gui::phone::active() {
                        "You're muted · tap to talk".to_owned()
                    } else {
                        format!("You're muted · {} to talk", crate::gui::shortcuts::label("⌘⇧M"))
                    },
                    theme::on_accent(),
                    theme::danger_fill(),
                )
                .cursor_pointer()
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .tooltip(tip("Unmute", Some("⌘⇧M")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if room_context {
                        this.tiny_toggle_mute(cx);
                    } else {
                        this.call_mute(cx);
                    }
                }))
                .into_any_element(),
            ),
            None => None,
        };
        let values = crate::gui::app::prefs::values();
        let push = values.on(protocol::settings::PUSH_TO_TALK) || ptt_only;
        let key = values.hotkey(protocol::settings::PTT_KEY).map(|binding| binding.label());
        let held = crate::gui::hotkeys::talk_held();
        #[cfg(feature = "gui-shot")]
        let (push, held, key) = match crate::gui::call::shot_ptt() {
            Some(down) => (true, down.then_some(false), Some("F".to_owned())),
            None => (push, held, key),
        };
        let ptt = match talk(push, held, key) {
            Talk::Open => None,
            Talk::Waiting(Some(key)) => Some(pill("tiny-pill-ptt", "icons/radio.svg", format!("Push to talk · hold {key}"), theme::text_soft(), theme::overlay())),
            Talk::Waiting(None) => Some(
                pill("tiny-pill-ptt", "icons/radio.svg", "Push to talk · pick a key in Settings › Voice".to_owned(), theme::warning(), theme::overlay())
                    .cursor_pointer()
                    .on_click(|_, _, cx| crate::gui::settings::open(crate::gui::settings::Section::Voice, cx)),
            ),
            Talk::Talking => Some(pill("tiny-pill-ptt", "icons/radio.svg", "Talking".to_owned(), theme::on_accent(), theme::positive())),
            Talk::Priority => Some(pill("tiny-pill-ptt", "icons/megaphone.svg", "Priority speaking".to_owned(), theme::on_accent(), theme::accent())),
        };
        let trouble = self
            .tiny_health()
            .and_then(|health| health.hints().into_iter().next())
            .filter(|hint| hint.level == health::Level::Bad)
            .map(|hint| {
                pill("tiny-pill-health", "icons/triangle-alert.svg", hint.words, theme::warning(), theme::overlay())
                    .max_w(px(520.))
                    .cursor_pointer()
                    .tab_index(0)
                    .focus_visible(|style| style.shadow(theme::focus_ring()))
                    .tooltip(tip("Call health", None))
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_health(cx)))
            });
        if muted.is_none() && ptt.is_none() && trouble.is_none() {
            return None;
        }
        Some(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .justify_center()
                .items_center()
                .gap(px(8.))
                .children(muted)
                .children(ptt)
                .children(trouble),
        )
    }

    /// The in-call control bar under the stage: mute, deafen, share, camera, devices, health, Leave.
    pub(in crate::gui) fn call_view_bar(&self, call: Option<&Call>, opts: &ViewBar, cx: &mut Context<Self>) -> Div {
        let bar = div().w_full().flex_none().pb(px(20.)).pt(px(8.)).flex().flex_col().items_center().gap(px(8.));
        let (muted, deafened) = if opts.room_context {
            self.tiny_quiet()
        } else {
            call.map_or((false, false), |c| (c.muted || c.deafened, c.deafened))
        };
        let connecting = call.is_some_and(|call| matches!(call.state, CallState::Connecting | CallState::RingingOut));
        let speaking = call.is_some_and(|call| call.state == CallState::Joined && call.speaking)
            && !muted
            && opts.held.is_none();
        let leave_word = if connecting { "Cancel" } else { "Leave" };
        let room_context = opts.room_context;
        let leave = div()
            .id("tiny-leave")
            .role(gpui::Role::Button)
            .aria_label(if connecting { "Cancel joining" } else { "Leave the call" })
            .h(px(44.))
            .px(px(20.))
            .rounded_full()
            .bg(theme::danger_fill())
            .hover(|style| style.bg(theme::danger()))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring_gap(theme::overlay())))
            .tooltip(tip(if connecting { "Cancel joining" } else { "Leave the call" }, Some("⌘⇧H")))
            .on_click(cx.listener(move |this, _, _, cx| {
                if room_context {
                    this.tiny_leave(cx);
                } else {
                    this.call_leave(cx);
                }
            }))
            .child(glyph("icons/phone-off.svg", 18., theme::on_accent()))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::on_accent()).child(leave_word))
            .into_any_element();
        let mute = match opts.held {
            Some(words) => bar_button("tiny-bar-mute", "icons/mic-off.svg", BarLook::Held).aria_label(words).tooltip(tip(words, None)),
            None => bar_button(
                "tiny-bar-mute",
                if muted { "icons/mic-off.svg" } else { "icons/mic.svg" },
                if muted { BarLook::Off } else { BarLook::Rest },
            )
            .aria_label(if muted { "Unmute" } else { "Mute" })
            .tooltip(tip(if muted { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
            .on_click(cx.listener(move |this, _, _, cx| {
                if room_context {
                    this.tiny_toggle_mute(cx);
                } else {
                    this.call_mute(cx);
                }
            })),
        }
        .when(speaking, |mute| {
            mute.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded_full()
                    .border_2()
                    .border_color(theme::positive()),
            )
        });
        let deafen = if opts.held_ears {
            bar_button("tiny-bar-deafen", "icons/headphone-off.svg", BarLook::Held)
                .aria_label("A moderator deafened you here")
                .tooltip(tip("A moderator deafened you here", None))
        } else {
            bar_button(
                "tiny-bar-deafen",
                if deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" },
                if deafened { BarLook::Off } else { BarLook::Rest },
            )
            .aria_label(if deafened { "Undeafen" } else { "Deafen" })
            .tooltip(tip(if deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
            .on_click(cx.listener(move |this, _, _, cx| {
                if room_context {
                    this.tiny_toggle_deafen(cx);
                } else {
                    this.call_deafen(cx);
                }
            }))
        };
        let joined = call.is_some_and(|call| call.state == CallState::Joined);
        let pictures = joined && opts.video_allowed;
        let share_on = call.is_some_and(|call| sending(call, Content::Share));
        let camera_on = call.is_some_and(|call| sending(call, Content::Camera));
        let share = pictures.then(|| {
            bar_button(
                "tiny-bar-share",
                "icons/monitor-up.svg",
                if share_on { BarLook::On } else { BarLook::Rest },
            )
            .aria_label(if share_on { "Stop sharing" } else { "Share your screen" })
            .tooltip(tip(if share_on { "Stop sharing" } else { "Share your screen" }, None))
            .on_click(cx.listener(move |this, _, window, cx| {
                if share_on {
                    this.tiny_stop_share(cx);
                } else {
                    this.tiny_share(window, cx);
                }
            }))
        });
        let camera = pictures.then(|| {
            let call_cam = call.cloned();
            bar_button(
                "tiny-bar-camera",
                "icons/video.svg",
                if camera_on { BarLook::On } else { BarLook::Rest },
            )
            .aria_label(if camera_on { "Turn camera off" } else { "Turn camera on" })
            .tooltip(tip(if camera_on { "Turn camera off" } else { "Turn camera on" }, None))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if let Some(call) = &call_cam {
                    this.call_camera_clicked(call, camera_on, cx);
                } else {
                    this.call_ask(Request::Media(MediaRequest::Camera { on: !camera_on }), cx);
                }
            }))
        });
        let popout_open = self.call_popout_open();
        let popout = pictures.then(|| {
            bar_button("tiny-bar-popout", "icons/picture-in-picture-2.svg", if popout_open { BarLook::On } else { BarLook::Rest })
                .aria_label(if popout_open { "Bring video back" } else { "Pop out video" })
                .tooltip(tip(if popout_open { "Bring video back" } else { "Pop out video" }, Some("⇧⌘O")))
                .on_click(cx.listener(|this, _, _, cx| this.call_toggle_popout(cx)))
        });
        let refused = self
            .call_refused()
            .map(str::to_owned)
            .or_else(|| call.and_then(|call| call.device_notice.clone()));
        let health = call
            .filter(|call| call.state == CallState::Joined && call.conversation.network == Network::Tiny)
            .and_then(|_| self.tiny_health())
            .map(|health| {
                let grade = health.grade();
                let words = match health.rtt_ms {
                    Some(ms) if grade != Grade::Unknown => {
                        format!("Call health: {} · {ms} ms", grade.words().to_lowercase())
                    }
                    _ => "Call health".to_owned(),
                };
                div()
                    .id("tiny-bar-health")
                    .role(gpui::Role::Button)
                    .aria_label(words.clone())
                    .size(px(44.))
                    .rounded_full()
                    .bg(theme::selected())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tab_index(0)
                    .hover(|style| style.bg(theme::pressed()))
                    .focus_visible(|style| style.shadow(theme::focus_ring()))
                    .child(health::bars(grade, 16.))
                    .tooltip(tip(words, None))
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_health(cx)))
            });
        let has_call = call.is_some();
        let menu = self.call_menu();
        let devices = bar_button(
            "tiny-bar-devices",
            "icons/settings.svg",
            if menu == Some(Menu::Devices) { BarLook::On } else { BarLook::Rest },
        )
        .aria_label("Devices")
        .tooltip(tip("Devices", None))
        .on_click(cx.listener(move |this, _, _, cx| {
            if has_call {
                cx.stop_propagation();
                this.call_menu_toggle(Menu::Devices, cx);
            } else {
                crate::gui::settings::open(crate::gui::settings::Section::Audio, cx);
            }
        }));
        let soundboard = call.filter(|call| joined && call.conversation.network == Network::Discord).map(|call| {
            let key = call.conversation.clone();
            bar_button(
                "call-bar-soundboard",
                "icons/audio-lines.svg",
                if menu == Some(Menu::Soundboard) { BarLook::On } else { BarLook::Rest },
            )
            .aria_label("Soundboard")
            .tooltip(tip("Soundboard", None))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.call_soundboard_toggle(key.clone(), cx);
            }))
        });
        let bar_popover = call.and_then(|call| match menu {
            Some(Menu::Devices) => Some((300., self.call_devices_menu(call, cx).p(px(6.)))),
            Some(Menu::CameraPreview) => Some((284., self.call_camera_preview_menu(call, cx).p(px(8.)))),
            Some(Menu::AddPeople) => Some((280., self.call_add_people_menu(call, cx).p(px(6.)))),
            Some(Menu::Soundboard) => Some((320., self.call_soundboard_body(call, cx).p(px(8.)))),
            Some(Menu::Person { account, user_id, name }) => {
                Some((260., self.call_person_menu(account, &user_id, &name, cx).p(px(6.))))
            }
            _ => None,
        });
        let call_error = room_context.then(|| self.tiny_state.call_error.clone()).flatten();
        bar.relative()
            .children(self.call_view_pills(opts.ptt_only, call, opts.held, opts.room_context, cx))
            .child(
                div()
                    .max_w_full()
                    .p(px(6.))
                    .rounded(px(24.))
                    .bg(theme::overlay())
                    .border_1()
                    .border_color(theme::overlay_edge())
                    .shadow(theme::shadow_popover())
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .items_center()
                    .gap(px(8.))
                    .child(mute)
                    .child(deafen)
                    .children(share)
                    .children(camera)
                    .children(popout)
                    .children(soundboard)
                    .child(devices)
                    .children(health)
                    .child(leave),
            )
            .when(opts.no_sound, |bar| {
                bar.child(
                    div()
                        .type_small()
                        .text_color(theme::text_faint())
                        .child("This server has no audio turned on, so you have a seat and no sound."),
                )
            })
            .when(
                opts.audio_up && !crate::gui::app::prefs::values().processing().echo_cancellation,
                |bar| bar.child(div().type_small().text_color(theme::text_faint()).child(HEADPHONES)),
            )
            .children(refused.map(|detail| {
                div()
                    .max_w(px(520.))
                    .whitespace_normal()
                    .type_small()
                    .text_color(theme::warning())
                    .child(detail)
            }))
            .children(call_error.map(|detail| {
                div().type_small().text_color(theme::danger()).child(detail)
            }))
            .children(bar_popover.map(|(width, body)| {
                deferred(
                    div()
                        .id("call-view-popover")
                        .absolute()
                        .bottom(relative(1.))
                        .left_0()
                        .right_0()
                        .pb(px(8.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w(px(width))
                                .rounded(px(12.))
                                .bg(theme::overlay())
                                .border_1()
                                .border_color(theme::border())
                                .shadow(theme::shadow_popover())
                                .occlude()
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.call_menu_close(cx)))
                                .child(body),
                        ),
                )
                .priority(1)
            }))
    }

    /// Activity tile, stage topic, and stage controls between the banner and the seats.
    fn call_discord_face(&self, call: &Call, seats: &[Seat], cx: &mut Context<Self>) -> Option<Div> {
        let action = super::discord_ux::stage_button(call.stage, call.request_to_speak, call.suppressed);
        let lower = super::discord_ux::stage_lower_hand(call.stage, call.request_to_speak, call.suppressed);
        if call.activity.is_none() && call.stage_topic.is_empty() && action.is_none() {
            return None;
        }
        let (mut speakers, mut raised, mut audience) = (0usize, 0usize, 0usize);
        if call.stage {
            for seat in seats {
                let hand = if seat.me { call.request_to_speak } else { call.raised.iter().any(|id| id == &seat.id) };
                match super::discord_ux::stage_place(seat.suppressed, hand) {
                    super::discord_ux::StageSeat::Speaker => speakers += 1,
                    super::discord_ux::StageSeat::Raised => raised += 1,
                    super::discord_ux::StageSeat::Audience => audience += 1,
                }
            }
        }
        let key = call.conversation.clone();
        let stage_controls = action.map(|action| {
            let ask = key.clone();
            let lower_key = key.clone();
            div()
                .flex()
                .gap(px(8.))
                .child(
                    button("call-stage-action", ButtonKind::Secondary, super::discord_ux::stage_button_label(action))
                        .h(px(28.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.call_ask(Request::StageVoice { conversation: ask.clone(), action, person: None }, cx);
                        })),
                )
                .when(lower, |row| {
                    row.child(
                        button("call-stage-lower", ButtonKind::Ghost, "Lower hand")
                            .h(px(28.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.call_ask(Request::StageVoice {
                                    conversation: lower_key.clone(),
                                    action: protocol::StageVoiceAction::Audience,
                                    person: None,
                                }, cx);
                            })),
                    )
                })
        });
        let activity = call.activity.clone();
        let topic = (!call.stage_topic.is_empty()).then(|| call.stage_topic.clone());
        Some(
            div()
                .w_full()
                .px(px(theme::PANE_PAD))
                .pt(px(8.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .children(activity.map(|activity| {
                    let names: Vec<String> = activity.participants.iter().map(|id| {
                        call.participants.iter().find(|who| &who.id == id).map(|who| who.name.clone()).unwrap_or_else(|| id.clone())
                    }).collect();
                    let who = names.join(", ");
                    let join = key.clone();
                    div()
                        .id("call-activity")
                        .max_w(px(420.))
                        .w_full()
                        .p(px(10.))
                        .rounded(px(theme::R_CARD))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(activity.icon.as_ref().and_then(|file| file.path.clone()).map(|path| {
                            img(path).size(px(28.)).rounded(px(6.))
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().type_ui().child(super::discord_ux::activity_line(&activity.name)))
                                .when(!who.is_empty(), |col| {
                                    col.child(div().truncate().type_small().text_color(theme::text_soft()).child(who))
                                }),
                        )
                        .child(
                            button_with_icon("call-activity-join", ButtonKind::Secondary, "icons/external-link.svg", "Join in Discord")
                                .h(px(28.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| this.call_handoff(join.clone(), cx))),
                        )
                }))
                .children(topic.map(|topic| div().type_small().text_color(theme::text_soft()).child(topic)))
                .when(call.stage, |col| {
                    col.child(
                        div()
                            .type_micro()
                            .text_color(theme::text_faint())
                            .child(super::discord_ux::stage_roster_line(speakers, raised, audience)),
                    )
                })
                .children(stage_controls),
        )
    }

    /// Searchable soundboard grid. Favorites sort first. A star keeps the sound in Settings.
    pub(in crate::gui) fn call_soundboard_body(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        use protocol::settings::{GLOBAL, SOUNDBOARD_FAVORITES};
        let query = self
            .call_state
            .sound_query
            .get_or_init(|| {
                let input = cx.new(|cx| crate::gui::input::TextInput::new("Search sounds", cx));
                cx.observe(&input, |_, _, cx| cx.notify()).detach();
                input
            })
            .clone();
        let words = query.read(cx).text().to_owned();
        let stored = crate::gui::app::prefs::values().stored(GLOBAL, SOUNDBOARD_FAVORITES).unwrap_or("").to_owned();
        let favorite_ids = super::discord_ux::favorite_ids(&stored);
        let shown = super::discord_ux::order_sounds(&self.call_state.sounds, &favorite_ids, &words);
        let filtered_empty = shown.is_empty();
        let key = call.conversation.clone();
        let mut grid = div().w_full().flex().flex_wrap().gap(px(6.));
        for sound in shown {
            let id = sound.id.clone();
            let guild = sound.guild_id.clone();
            let starred = favorite_ids.iter().any(|known| known == &id);
            let label = if sound.emoji.is_empty() { sound.name.clone() } else { format!("{} {}", sound.emoji, sound.name) };
            let play_id = id.clone();
            let play_key = key.clone();
            let play_guild = guild;
            let star_id = id.clone();
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("sound-{id}")))
                    .w(px(92.))
                    .min_h(px(64.))
                    .px(px(6.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .bg(theme::hover())
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::selected()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.call_ask(
                            Request::PlaySoundboard { conversation: play_key.clone(), sound_id: play_id.clone(), guild_id: play_guild.clone() },
                            cx,
                        );
                    }))
                    .child(div().max_w_full().truncate().type_small().child(label))
                    .child(
                        div()
                            .id(SharedString::from(format!("sound-star-{id}")))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_soundboard_favorite(&star_id, cx);
                            }))
                            .child(glyph("icons/star.svg", 12., if starred { theme::accent() } else { theme::text_faint() })),
                    ),
            );
        }
        let empty = if self.call_state.sound_loading {
            "Loading sounds…"
        } else if self.call_state.sounds.is_empty() {
            "No sounds"
        } else if filtered_empty {
            "No matching sounds"
        } else {
            ""
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).child("Soundboard"))
            .child(query)
            .child(
                div()
                    .id("call-soundboard-grid")
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .child(if empty.is_empty() { grid.into_any_element() } else {
                        div().type_small().text_color(theme::text_faint()).child(empty).into_any_element()
                    }),
            )
    }

    /// A click on a tile: that person large, or everyone alike again.
    pub(in crate::gui) fn call_toggle_focus(&mut self, user_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.call_keep_keys(window, cx);
        self.call_state.focus = stage::toggle(self.call_state.focus.as_ref(), Focus::Person(user_id.to_owned()));
        self.call_sync_watch_now(cx);
        cx.notify();
    }

    /// A click on a stream: watched, and filling the stage; when it already fills it, the grid.
    pub(in crate::gui) fn call_open_stream(
        &mut self,
        user_id: &str,
        mine: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.call_keep_keys(window, cx);
        let focus = stage::toggle(self.call_state.focus.as_ref(), Focus::Stream(user_id.to_owned()));
        let active_call = self.snapshot.as_ref().and_then(|s| self.call_shown(s));
        if focus.is_some() && !mine && !active_call.is_some_and(|call| watching(&call, user_id)) {
            self.call_watch(user_id.to_owned(), cx);
        }
        self.call_state.focus = focus;
        self.call_sync_watch_now(cx);
        cx.notify();
    }

    pub(in crate::gui) fn call_stop_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.call_keep_keys(window, cx);
        if matches!(self.call_state.focus, Some(Focus::Stream(_))) {
            self.call_state.focus = None;
        }
        self.call_unwatch(cx);
        self.call_sync_watch_now(cx);
    }

    /// The people under the focused stream, over it, or gone.
    pub(in crate::gui) fn call_set_strip(&mut self, strip: stage::Strip, window: &mut Window, cx: &mut Context<Self>) {
        self.call_keep_keys(window, cx);
        self.call_state.strip = strip;
        cx.notify();
    }

    /// The focused stream has the whole window, which goes full screen if it was not.
    pub(in crate::gui) fn call_full(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.call_state.full.is_some() {
            return;
        }
        self.call_keep_keys(window, cx);
        let took = !window.is_fullscreen();
        if took {
            window.toggle_fullscreen();
        }
        self.call_state.full = Some(took);
        cx.notify();
    }

    /// Out of the stream's full screen, and the window back as it was. False when it was not.
    pub(in crate::gui) fn call_unfull(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(took) = self.call_state.full.take() else {
            return false;
        };
        self.call_keep_keys(window, cx);
        if took && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
        true
    }

    /// Steps Escape back out of the call view: first clear stage focus, then leave the call view.
    pub(in crate::gui) fn call_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.call_state.focus.take().is_some() {
            self.call_sync_watch_now(cx);
            cx.notify();
            return true;
        }
        if self.call_state.view.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    /// Keep keyboard focus on the window root so Escape still reaches `close_top`.
    pub(in crate::gui) fn call_keep_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }
}
