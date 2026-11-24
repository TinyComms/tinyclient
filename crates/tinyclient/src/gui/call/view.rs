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
use protocol::{
    Call, CallState, Content, Conversation, ConversationKind, Key, MediaRequest, Network,
    PictureKind, Request,
};
use std::collections::HashMap;

const STAGE_PAD: f32 = 24.;
const TILE_GAP: f32 = 12.;
/// A focused tile: the one person drawn large.
const FOCUS_W: f32 = 420.;
const FOCUS_H: f32 = 300.;
/// A tile in the strip under a focused stream.
const STRIP_W: f32 = 150.;
const STRIP_H: f32 = 96.;

/// Size a person tile for the available stage width, including its outer padding.
pub(crate) fn stage_tile_size(avail_w: f32, seat_count: usize, has_streams: bool) -> (f32, f32) {
    if has_streams {
        return (STRIP_W, STRIP_H);
    }
    let inner = avail_w - 2. * STAGE_PAD;
    let two = ((inner - TILE_GAP) / 2.).floor();
    let (w, min_h, max_h) = match seat_count {
        0 | 1 => (inner.floor().clamp(180., 320.), 140., 240.),
        2 if avail_w >= 396. => (two.clamp(168., 280.), 132., 210.),
        2 => (inner.floor().clamp(160., 260.), 124., 196.),
        3 | 4 => (two.clamp(164., 248.), 126., 186.),
        _ => {
            let cols = if avail_w >= 640. { 3. } else { 2. };
            let w = ((inner - TILE_GAP * (cols - 1.)) / cols).floor();
            (w.clamp(148., 210.), 114., 158.)
        }
    };
    (w, (w * 0.75).round().clamp(min_h, max_h))
}

/// Center a stage that fits and let a taller stage grow from the scroll viewport's top.
fn centered_stage() -> Div {
    div()
        .min_h_full()
        .w_full()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .p(px(STAGE_PAD))
        .gap(px(20.))
}

/// Said under the call while echo cancellation is switched off in Settings → Voice.
pub(in crate::gui) const HEADPHONES: &str =
    "Echo cancellation is off: wear headphones, or others hear themselves.";

/// How large a tile is drawn.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::gui) enum Size {
    Grid(f32, f32),
    Big(f32),
    Strip,
}

impl Size {
    /// A person's tile: width, height and avatar.
    pub(in crate::gui) fn person(self) -> (f32, f32, f32) {
        match self {
            Size::Grid(w, h) => (w, h, (h * 0.40).round()),
            Size::Big(w) => {
                let h = (w * FOCUS_H / FOCUS_W).round();
                (w, h, (h * 0.37).round())
            }
            Size::Strip => (STRIP_W, STRIP_H, 36.),
        }
    }

    /// A stream's tile: as tall as a person's, 16:9.
    pub(in crate::gui) fn stream(self) -> (f32, f32) {
        let (_, h, _) = self.person();
        (h * 16. / 9., h)
    }

    fn grid(avail_w: f32, seat_count: usize, has_streams: bool) -> Self {
        let (w, h) = stage_tile_size(avail_w, seat_count, has_streams);
        if has_streams { Self::Strip } else { Self::Grid(w, h) }
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

/// Split-pill call bar button (Discord-style): left main action + right chevron device picker.
/// Both halves share one 44 px pill container; right-clicking anywhere on the pill also opens the
/// device picker (attached by the caller).
fn bar_split_button(
    pill_id: &'static str,
    main_id: &'static str,
    chevron_id: &'static str,
    icon: &'static str,
    look: BarLook,
    menu_open: bool,
    main_label: SharedString,
    main_shortcut: Option<&'static str>,
    chevron_label: &'static str,
    on_main: Option<Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static>>,
    on_menu: Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static>,
    on_right: Box<dyn Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static>,
) -> Stateful<Div> {
    let (fill, color) = match look {
        BarLook::Rest => (theme::selected(), theme::text()),
        BarLook::Off | BarLook::Held => (theme::danger_soft(), theme::danger()),
        BarLook::On => (theme::accent_soft(), theme::accent()),
    };
    let chevron_icon = if menu_open { "icons/chevron-down.svg" } else { "icons/chevron-up.svg" };
    let main_half = div()
        .id(main_id)
        .role(gpui::Role::Button)
        .aria_label(main_label.clone())
        .h_full()
        .pl(px(13.))
        .pr(px(9.))
        .rounded_l_full()
        .flex()
        .items_center()
        .justify_center()
        .tooltip(tip(main_label, main_shortcut))
        .map(|button| match (look, on_main) {
            (BarLook::Held, _) | (_, None) => button.opacity(0.55).aria_disabled(true),
            (_, Some(handler)) => button
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.bg(theme::pressed()))
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .on_click(move |ev, window, cx| handler(ev, window, cx)),
        })
        .child(glyph(icon, 20., color));
    let divider = div()
        .flex_none()
        .w(px(1.))
        .h(px(20.))
        .bg(theme::hairline());
    let chevron_half = div()
        .id(chevron_id)
        .role(gpui::Role::Button)
        .aria_label(chevron_label)
        .h_full()
        .pl(px(6.))
        .pr(px(10.))
        .rounded_r_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .when(menu_open, |btn| btn.bg(theme::pressed()))
        .hover(|style| style.bg(theme::pressed()))
        .focus_visible(|style| style.shadow(theme::focus_ring()))
        .tooltip(tip(chevron_label, None))
        .on_click(move |ev, window, cx| on_menu(ev, window, cx))
        .child(glyph(chevron_icon, 13., color));
    div()
        .id(pill_id)
        .relative()
        .h(px(44.))
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| on_right(ev, window, cx))
        .child(main_half)
        .child(divider)
        .child(chevron_half)
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
fn stream_button(id: impl Into<gpui::ElementId>, icon: &'static str, words: impl Into<SharedString>) -> Stateful<Div> {
    let words: SharedString = words.into();
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(words.clone())
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

/// Label at a tile or stream's foot. Screen shares carry the red LIVE mark; webcam tiles show
/// the person's name pill (including in the compact Strip row) without LIVE.
fn caption(words: String, small: bool, live: bool, lift_for_strip: bool) -> Div {
    div()
        .absolute()
        .bottom(px(if lift_for_strip { 136. } else if small { 6. } else { 10. }))
        .left(px(if small { 6. } else { 10. }))
        .max_w(px(if small { 138. } else { 360. }))
        .flex()
        .items_center()
        .gap(px(6.))
        .when(live, |row| {
            row.child(chip("LIVE", theme::on_accent(), theme::danger_fill()).font_weight(theme::SEMIBOLD))
        })
        .when(!small || !live, |row| {
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

    /// Whether the main pane is currently rendering `call_view_bar` for the active call (either
    /// the standalone call view pane or the active Tiny voice room stage).
    pub(in crate::gui) fn call_stage_bar_visible(&self, snapshot: &Snapshot) -> bool {
        if self.call_view_active(snapshot) {
            return true;
        }
        let Some(call) = self.call_shown(snapshot) else {
            return false;
        };
        snapshot.open.is_none()
            && self.tiny_open()
            && self.tiny_state.account.as_ref() == Some(&call.conversation.account())
            && self.tiny_state.channel.as_deref() == Some(call.conversation.id.as_str())
            && self.tiny_room(&call.conversation).is_some()
    }

    /// Opens the main-pane call view for `key`.
    pub(in crate::gui) fn call_open_view(&mut self, key: Key, cx: &mut Context<Self>) {
        self.call_state.view = Some(key);
        cx.notify();
    }

    fn call_conversation_row(&self, snapshot: &Snapshot, call: &Call) -> Option<Conversation> {
        snapshot.open.as_ref().filter(|row| row.key == call.conversation).cloned()
            .or_else(|| self.sidebar_state.find(&self.trees, &call.conversation))
    }

    /// Builds a `CallPeople` lookup for a non-room call, including its local and ringing seats.
    fn call_people_for(&self, call: &Call, stage: &Stage, subject: &Subject, cx: &mut Context<Self>) -> CallPeople {
        let account = call.conversation.account();
        let mut names = HashMap::new();
        let mut pictures = HashMap::new();
        for seat in &stage.seats {
            names.insert(seat.id.clone(), seat.name.clone());
            let who = call.participants.iter().find(|who| who.id == seat.id && who.ringing == seat.ringing);
            let picture = self.tiny_member_picture(&account, &seat.id)
                .or_else(|| who.and_then(|who| self.call_participant_picture(&account, who, cx)))
                .or_else(|| {
                    if seat.me {
                        self.person_picture(&Key::new(&account, &seat.id), cx).or_else(|| {
                            (account.network == Network::Tiny)
                                .then(|| crate::gui::app::prefs::me().picture_path())
                                .flatten()
                        })
                    } else if subject.kind == ConversationKind::Direct {
                        let file = subject.avatar.as_ref()?;
                        self.people_state.hub().file(&account, file, cx).path.map(Into::into)
                    } else {
                        None
                    }
                });
            if let Some(path) = picture {
                pictures.insert(seat.id.clone(), path);
            }
        }
        CallPeople { conversation: call.conversation.clone(), names, pictures }
    }

    /// My account's id and display name for `call`.
    fn call_me(&self, snapshot: &Snapshot, call: &Call) -> (String, String) {
        let account = call.conversation.account();
        let found = snapshot.accounts.iter().find(|a| a.key == account);
        let page = self.tiny_page_facts().filter(|page| page.account == &account);
        let id = page.as_ref().map(|page| page.community.me.as_str()).filter(|id| !id.is_empty())
            .or_else(|| found.map(|a| a.me.as_str()).filter(|id| !id.is_empty()))
            .unwrap_or("me")
            .to_owned();
        let name = if account.network == Network::Tiny {
            page.and_then(|page| page.community.member(&id))
                .map(crate::gui::tiny::model::name_of)
                .or_else(|| {
                    self.people_state.hub().person(&Key::new(&account, &id))
                        .map(|person| person.name)
                        .filter(|name| !name.trim().is_empty())
                })
                .unwrap_or_else(|| {
                    let me = crate::gui::app::prefs::me();
                    if me.name.trim().is_empty() { "You".to_owned() } else { me.name }
                })
        } else {
            found.map(|a| a.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| "You".to_owned())
        };
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
        let row = self.call_conversation_row(snapshot, &call);
        let peer = row.as_ref().and_then(|row| row.peer.as_deref());
        let (me_id, me_name) = self.call_me(snapshot, &call);
        let mut stage = Stage::of_call(&call, &subject, &me_id, &me_name, peer);
        if call.stage {
            super::discord_ux::order_seats(&mut stage.seats, &call.raised, call.request_to_speak);
        }
        let people = self.call_people_for(&call, &stage, &subject, cx);

        let phone = crate::gui::phone::active();
        let end_pad = chrome::end_pad(End::Conversation, 12.);
        let roomy = self.conversation_room() - end_pad >= 540.;
        let count = stage.seats.iter().filter(|seat| !seat.ringing).count();
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

        let unread = row.as_ref().map_or(0, |row| row.unread);
        let show_chat = button_with_icon(
            "call-view-show-chat",
            ButtonKind::Secondary,
            "icons/message-square.svg",
            "Show chat",
        )
        .flex_none()
        .h(px(28.))
        .when(unread > 0, |button| {
            let count = if unread > 99 { "99+".to_owned() } else { unread.to_string() };
            button.child(chip(count, theme::accent_text(), theme::accent_soft()).rounded_full())
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _, _, cx| this.call_dismiss_view(cx)));
        let title_icon = if subject.kind == ConversationKind::Voice {
            "icons/volume-2.svg"
        } else {
            "icons/phone.svg"
        };
        let voice_status = row.as_ref().and_then(crate::gui::sidebar::discord_voice_status);
        let voice_details = call.conversation.network == Network::Discord && call.state == CallState::Joined
            && subject.kind == ConversationKind::Voice;
        let details_key = call.conversation.clone();
        let title_row = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(glyph(
                title_icon,
                16.,
                if call.state == CallState::Joined { theme::positive() } else { theme::text_soft() },
            ))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(260.))
                    .truncate()
                    .type_title()
                    .text_color(theme::text())
                    .child(subject.title.clone()),
            )
            .children(place_chip.filter(|_| roomy))
            .children(voice_status.map(|status| self.voice_status_line(&call.conversation, &status, cx)))
            .when(roomy, |row| row.child(chip(count_label, theme::text_soft(), theme::hover())))
            .children(
                call.record_start_date
                    .map(|since| super::telegram::recording_badge("call-header-rec", since, call.record_video_active)),
            )
            .children(
                call.rtmp_stream
                    .then(|| chip("Live stream", theme::accent_text(), theme::accent_soft())),
            )
            .children(self.call_stage_clock(&call, cx).map(|clock| {
                div().flex_none().type_small().text_color(theme::text_soft()).child(clock)
            }))
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
        let streams = stage::streams(&stage, &me_id, sending_now(Some(&call)));
        let auto_peer_video = self.call_state.grid_choice_for.as_ref() != Some(&call.conversation);
        let has_peer_video = self.call_peer_video(&stage, &streams, Some(&call)).is_some();
        let focused = stage::still(self.call_state.focus.as_ref(), &stage, &streams).is_some()
            || (auto_peer_video && has_peer_video);
        let layout_key = call.conversation.clone();
        let layout_toggle = (focused || stage.seats.len() > 1).then(|| {
            let target = streams.iter().find(|stream| !stream.mine && stream.watchable)
                .or_else(|| streams.iter().find(|stream| stream.mine))
                .cloned();
            let person = stage.seats.iter().find(|seat| !seat.me && !seat.ringing)
                .or_else(|| stage.seats.iter().find(|seat| !seat.me))
                .map(|seat| seat.id.clone());
            let (icon, label) = if focused { ("icons/users.svg", "Grid") } else { ("icons/maximize.svg", "Focus") };
            let button = if roomy {
                button_with_icon("call-view-layout", ButtonKind::Ghost, icon, label).h(px(28.))
            } else {
                icon_button("call-view-layout", icon, false)
            };
            button
                .flex_none()
                .tooltip(tip(label, None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if focused {
                        this.call_state.grid_choice_for = Some(layout_key.clone());
                        this.call_keep_keys(window, cx);
                        this.call_state.focus = None;
                        this.call_sync_watch_now(cx);
                        cx.notify();
                    } else if has_peer_video {
                        this.call_state.grid_choice_for = None;
                        this.call_state.focus = None;
                        this.call_keep_keys(window, cx);
                        cx.notify();
                    } else if let Some(stream) = &target {
                        this.call_open_stream(&stream.user_id, stream.mine, window, cx);
                    } else if let Some(id) = &person {
                        this.call_toggle_focus(id, window, cx);
                    }
                }))
        });
        let popout = (!phone && super::in_call(&call)).then(|| {
            let open = self.call_popout_open();
            icon_button("call-view-popout", "icons/picture-in-picture-2.svg", open)
                .tooltip(tip(if open { "Bring back to window" } else { "Pop out" }, Some("⇧⌘O")))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.call_toggle_popout(cx)))
        });
        let bar = drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .pl(px(theme::PANE_PAD))
            .pr(px(end_pad))
            .border_b_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .when(!phone, |bar| bar.child(self.strip_controls(window, cx)))
            .child(show_chat)
            .child(title_row)
            .children(voice_details.then(|| icon_button("call-voice-region", "icons/info.svg", false)
                .tooltip(tip("Connection details", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_discord_voice_settings(details_key.clone(), false, cx);
                }))))
            .children(add_people)
            .children(layout_toggle)
            .children(popout)
            .when(!cfg!(target_os = "macos") && end_pad > 12., |bar| {
                bar.child(div().flex_none().w(px(1.)).h(px(20.)).bg(theme::hairline()))
            });

        let held = if call.server_muted {
            Some("A moderator muted you here")
        } else if call.suppressed {
            Some("You can't speak in this channel")
        } else {
            None
        };
        let banner = self.call_view_banner_inner(&subject.title, &call, false, cx);
        let stage_el = self.call_stage_inner(&stage, &people, Some(&call), false, cx);
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
        self.call_view_banner_inner(title_place, call, true, cx)
    }

    fn call_view_banner_inner(
        &self,
        title_place: &str,
        call: &Call,
        show_clock: bool,
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
            CallState::Joined if call.rtmp_stream => {
                let msg = call
                    .rtmp_state
                    .and_then(super::telegram::rtmp_state_banner)?;
                ("icons/radio.svg", theme::warning(), theme::warning_soft(), msg.to_owned(), None)
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
            let connecting = matches!(call.state, CallState::Connecting | CallState::RingingOut);
            let word = if connecting { "Cancel" } else { "Leave" };
            Some(
                button_with_icon("tiny-banner-leave", ButtonKind::Danger, "icons/phone-off.svg", word)
                    .h(px(30.))
                    .tooltip(tip(word, Some("⌘⇧H")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if connecting {
                            this.call_leave(cx);
                        } else {
                            this.telegram_leave_or_end(cx);
                        }
                    }))
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
        let clock = show_clock.then(|| self.call_stage_clock(call, cx)).flatten();
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
        self.call_stage_inner(stage, people, call, room_context, cx)
    }

    fn call_peer_video<'a>(&self, stage: &Stage, streams: &'a [Stream], call: Option<&Call>) -> Option<&'a Stream> {
        if stage.kind != ConversationKind::Direct || stage.seats.len() > 2 {
            return None;
        }
        streams.iter().find(|stream| {
            let kind = if stream.share { PictureKind::Share } else { PictureKind::Camera };
            !stream.mine && self.call_frame(call, &stream.user_id, kind).is_some()
        })
    }

    fn call_stage_inner(
        &self,
        stage: &Stage,
        people: &dyn People,
        call: Option<&Call>,
        room_context: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let me = stage.me_id().unwrap_or("");
        let streams = stage::streams(stage, me, sending_now(call));
        let has_share_tiles = streams.iter().any(stage::separate_stream_tile);
        let auto_peer_video = call.is_none_or(|c| self.call_state.grid_choice_for.as_ref() != Some(&c.conversation));
        #[cfg(feature = "gui-shot")]
        let shot_focus = super::shot::focus();
        #[cfg(not(feature = "gui-shot"))]
        let shot_focus: Option<Focus> = None;
        let active_focus = shot_focus.as_ref().or(self.call_state.focus.as_ref());
        let focus = stage::still(active_focus, stage, &streams);
        let mut avail_w = self.conversation_room();
        if room_context {
            // Match the Tiny pane's drawer threshold. The width in `conversation_room` already
            // accounts for the rail and the sidebar when it has its own column.
            if self.layout_state.sidebar_hidden && !self.layout_state.floating && !crate::gui::phone::active() {
                avail_w -= 260.;
            }
            if self.tiny_state.members_open() && avail_w - 248. >= 440. {
                avail_w -= 248.;
            }
        }
        let base = div().id("tiny-stage").flex_1().w_full().min_h_0();
        match focus {
            None => {
                // Direct 1:1 video call: if the peer has a live frame on screen, draw it large
                // with my own camera/tile in the corner.
                if let Some(peer_stream) = auto_peer_video.then(|| self.call_peer_video(stage, &streams, call)).flatten() {
                    let main = self.call_stream_view(stage, people, peer_stream, call, false, true, cx);
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
                let (grid_seats, roster_seats) =
                    stage::partition_stage(&stage.seats, &streams, me, stage::STAGE_GRID_CAP);
                let size = Size::grid(avail_w, grid_seats.len(), has_share_tiles);
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
                let mut content = centered_stage()
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .content_center()
                            .items_center()
                            .justify_center()
                            .gap(px(TILE_GAP))
                            .children(grid_tiles),
                    );
                if !roster_seats.is_empty() {
                    let mut roster = div()
                        .flex_none()
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
                crate::gui::components::scroll::scroll_page(
                    "tiny-stage-grid-scroll-frame",
                    base.overflow_y_scroll().child(content),
                )
                .into_any_element()
            }
            Some(Focus::Person(focus)) => {
                let big_size = Size::Big((avail_w - 2. * STAGE_PAD).clamp(160., FOCUS_W));
                let big = stage
                    .seats
                    .iter()
                    .find(|seat| seat.id == *focus)
                    .map(|seat| self.call_tile(stage, people, seat, call, big_size, room_context, cx));
                let rest = self.call_tiles(
                    stage,
                    people,
                    &streams,
                    call,
                    Some(&Focus::Person(focus.clone())),
                    Size::grid(avail_w, stage.seats.len().saturating_sub(1), has_share_tiles),
                    room_context,
                    cx,
                );
                crate::gui::components::scroll::scroll_page(
                    "tiny-stage-person-scroll-frame",
                    base.overflow_y_scroll().child(
                        centered_stage()
                            .gap(px(16.))
                            .children(big)
                            .when(!rest.is_empty(), |content| {
                                content.child(
                                    div().flex_none().w_full().flex().flex_wrap()
                                        .justify_center().gap(px(TILE_GAP)).children(rest),
                                )
                            }),
                    ),
                )
                .into_any_element()
            }
            Some(Focus::Stream(user_id)) => {
                let Some(stream) = streams.iter().find(|stream| stream.user_id == *user_id) else {
                    return base.into_any_element();
                };
                let strip = self.call_state.strip;
                let view = self.call_stream_view(stage, people, stream, call, false, false, cx);
                let row = div()
                    .id("tiny-strip")
                    .max_w_full()
                    .overflow_x_scroll()
                    .py(px(4.))
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
                .find(|stream| stream.user_id == seat.id && stage::separate_stream_tile(stream))
                .filter(|stream| shown(Focus::Stream(stream.user_id.clone())))
            {
                tiles.push(self.call_stream_tile(stage, people, stream, call, size, cx));
            }
        }
        // My stream before the stage lists my seat.
        for stream in streams
            .iter()
            .filter(|stream| stage::separate_stream_tile(stream) && !seats.iter().any(|seat| seat.id == stream.user_id))
        {
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
        lift_stop_for_strip: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = if stream.share { PictureKind::Share } else { PictureKind::Camera };
        let asked = call.is_some_and(|call| watching(call, &stream.user_id));
        let pausing = stream.share
            && !stream.mine
            && call.is_some_and(|call| call.stream_paused)
            && (asked || self.call_state.watch_asked.as_ref().is_some_and(|(id, _)| id == &stream.user_id));
        if let Some(frame) = self.call_frame(call, &stream.user_id, kind) {
            if pausing {
                return div()
                    .size_full()
                    .relative()
                    .child(img(frame).size_full().object_fit(ObjectFit::Contain))
                    .child(
                        div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().child(
                            div()
                                .h(px(30.))
                                .px(px(14.))
                                .rounded_full()
                                .bg(theme::overlay())
                                .flex()
                                .items_center()
                                .type_small()
                                .font_weight(theme::SEMIBOLD)
                                .text_color(theme::text_soft())
                                .child(stage::watch_words(true, true)),
                        ),
                    )
                    .into_any_element();
            }
            return img(frame).size_full().object_fit(ObjectFit::Contain).into_any_element();
        }
        let small = size == Size::Strip;
        let large = matches!(size, Size::Big(_));
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
                        let stop_box = div()
                            .p(px(2.))
                            .rounded(px(10.))
                            .bg(theme::overlay())
                            .child(stop.h(px(28.)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()));
                        let positioned = if matches!(size, Size::Grid(..)) {
                            div().absolute().top(px(8.)).right(px(8.)).child(stop_box)
                        } else {
                            div()
                                .absolute()
                                .bottom(px(if lift_stop_for_strip { 136. } else { 10. }))
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .child(stop_box)
                        };
                        tile.child(positioned)
                    })
                    .when_some(stream.share.then(|| call.and_then(|call| stage::viewer_line(&call.viewers))).flatten(), |tile, line| {
                        tile.child(
                            div().absolute().top(px(8.)).left(px(8.)).max_w(px(if matches!(size, Size::Grid(..)) { 130. } else { 280. })).child(
                                div()
                                    .h(px(22.))
                                    .px(px(8.))
                                    .rounded_full()
                                    .bg(theme::overlay())
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .type_micro()
                                            .text_color(theme::text())
                                            .child(line),
                                    ),
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
                        theme::on_accent(),
                    ))
                })
                .when(!small, |body| {
                    body.child(
                        div()
                            .text_center()
                            .whitespace_normal()
                            .type_ui()
                            .font_weight(theme::SEMIBOLD)
                            .text_color(theme::on_accent())
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
                            .text_color(gpui::rgba(0xffffffb3))
                            .child(line),
                    )
                })
                .when_some(stream.share.then(|| call.and_then(|call| stage::viewer_line(&call.viewers))).flatten(), |body, line| {
                    body.child(div().text_center().whitespace_normal().type_small().text_color(gpui::rgba(0xffffffb3)).child(line))
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
                .child(div().type_small().text_color(gpui::rgba(0xffffffb3)).child("Camera on"))
                .into_any_element();
        }
        let since = self
            .call_state
            .watch_asked
            .as_ref()
            .filter(|(id, _)| *id == stream.user_id)
            .map(|(_, at)| at.elapsed())
            .unwrap_or_default();
        let trouble = (asked && !pausing)
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
                        .text_color(gpui::rgba(0xffffffb3))
                        .child(words),
                )
                .children(retry)
                .into_any_element();
        }
        let pill = div()
            .h(px(if small { 22. } else { 30. }))
            .px(px(if small { 8. } else { 14. }))
            .rounded_full()
            .bg(if asked || pausing { theme::overlay() } else { theme::accent() })
            .flex()
            .items_center()
            .gap(px(6.))
            .when(!asked && !pausing && !small, |pill| pill.child(glyph("icons/play.svg", 12., theme::on_accent())))
            .child(
                div()
                    .map(|words| if small { words.type_micro() } else { words.type_small() })
                    .font_weight(theme::SEMIBOLD)
                    .text_color(if asked || pausing { theme::text_soft() } else { theme::on_accent() })
                    .child(stage::watch_words(asked, pausing)),
            );
        let preview = (!asked && !pausing && stream.share).then(|| {
            call.and_then(|call| call.participants.iter().find(|person| person.id == stream.user_id && !person.preview.is_empty()).map(|person| person.preview.clone()))
        }).flatten();
        body.when_some(preview, |body, path| {
            body.child(img(std::path::PathBuf::from(path)).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Cover))
        })
        .when(!small, |body| {
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
        let unwatch_id = stream.user_id.clone();
        let can_unwatch_tile = stream.share
            && !stream.mine
            && live
            && call.is_some_and(|c| c.conversation.network == Network::Discord);
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
            .child(self.call_stream_body(people, stream, call, size, false, cx))
            .when(can_unwatch_tile, |tile| {
                tile.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .child(
                            stream_button(
                                SharedString::from(format!("discord-stream-unwatch-{unwatch_id}")),
                                "icons/x.svg",
                                format!("Stop watching {name}'s screen"),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.call_stop_watching_user(&unwatch_id, window, cx);
                            })),
                        ),
                )
            })
            .child(caption(
                if mine { format!("Your {what}") } else { format!("{name}'s {what}") },
                small,
                stream.share,
                false,
            ))
            .into_any_element()
    }

    /// The focused stream, filling the stage (or, `full`, the window).
    #[allow(clippy::too_many_arguments)]
    fn call_stream_view(
        &self,
        _stage: &Stage,
        people: &dyn People,
        stream: &Stream,
        call: Option<&Call>,
        full: bool,
        direct_auto: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let strip = self.call_state.strip;
        let lift_for_strip = !full && !direct_auto && strip.over && !strip.hidden;
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
            if !direct_auto {
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
                    });
            }
            tools = tools
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
        let unwatch_id = user_id.clone();
        if !mine {
            tools = tools.child(stream_button("tiny-stream-unwatch", "icons/x.svg", "Stop watching").on_click(
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.call_unfull(window, cx);
                    this.call_stop_watching_user(&unwatch_id, window, cx);
                }),
            ));
        }
        let popped_out = self.call_popout_open();
        // The pop-out window has the frame: the stage never draws the same picture twice.
        let body: AnyElement = if popped_out {
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
            self.call_stream_body(people, stream, call, Size::Big(FOCUS_W), lift_for_strip, cx).into_any_element()
        };
        let volume = call
            .filter(|_| stream.share && !mine)
            .and_then(|call| self.call_stream_volume(call, &user_id, cx))
            .map(|controls| controls.absolute().top(px(10.)).left(px(10.)));
        let call_key = call.map(|c| c.conversation.clone());
        div()
            .id(if full { "tiny-stream-whole" } else { "tiny-stream-focused" })
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .when(full, |view| view.size_full())
            .when(!full, |view| view.rounded(px(12.)).border_1().border_color(theme::hairline()))
            .bg(if popped_out { theme::raised() } else { gpui::rgb(0x000000) })
            .overflow_hidden()
            .when(!full, |view| {
                view.cursor_pointer()
                    .tooltip(tip("Click to see everyone again (Esc)", None))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if direct_auto {
                            this.call_state.grid_choice_for = call_key.clone();
                            this.call_state.focus = None;
                            this.call_keep_keys(window, cx);
                            cx.notify();
                        } else {
                            this.call_open_stream(&user_id, mine, window, cx);
                        }
                    }))
            })
            .child(body)
            .children(volume)
            .child(caption(
                if mine { format!("Your {what}") } else { format!("{name}'s {what}") },
                false,
                stream.share,
                lift_for_strip,
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
        let row = self.call_conversation_row(snapshot, &call);
        let peer = row.as_ref().and_then(|row| row.peer.as_deref());
        let (me_id, me_name) = self.call_me(snapshot, &call);
        let stage = Stage::of_call(&call, &subject, &me_id, &me_name, peer);
        let people = self.call_people_for(&call, &stage, &subject, cx);
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
            _ => self.call_peer_video(stage, &streams, call),
        };
        let has_stream = stream.is_some();
        let inside: AnyElement = match stream {
            Some(stream) => self.call_stream_view(stage, people, stream, call, true, false, cx).into_any_element(),
            None => div()
                .id("call-full-ended-scroll")
                .flex_1()
                .w_full()
                .h_full()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    centered_stage()
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
                        ),
                )
                .into_any_element(),
        };
        div()
            .id("tiny-full")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(if has_stream { gpui::rgb(0x000000) } else { theme::raised() })
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
        let big = matches!(size, Size::Big(_));
        let small = size == Size::Strip;
        let me = seat.me;
        let ringing = seat.ringing;
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
        let effect = call.and_then(|call| call.effect.as_ref()).filter(|effect| {
            effect.user_id == seat.id && super::discord_ux::effect_visible(effect.at, super::now())
        });
        let has_state = states.first().is_some()
            || (local_mute && !me)
            || effect.is_some()
            || (percent != 100 && !local_mute && !me);
        let state_row = has_state.then(|| {
            div()
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
                .children(effect.map(|effect| chip(super::discord_ux::effect_label(&effect.emoji), theme::accent_text(), theme::accent_soft())))
                .when(percent != 100 && !local_mute && !me, |row| {
                    row.child(chip(format!("{percent} %"), theme::text_soft(), theme::hover()))
                })
        });
        let stop_ring = (ringing && stage.key.network == Network::Discord && call.is_some_and(|call| {
            call.participants.iter().any(|who| who.id == seat.id && who.ringing)
        })).then(|| {
            let id = seat.id.clone();
            button(format!("call-stage-stop-ringing-{id}"), ButtonKind::Ghost, "Stop")
                .h(px(22.))
                .px(px(6.))
                .type_micro()
                .tooltip(tip("Stop ringing", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_ask(Request::CallStopRinging { people: Some(vec![id.clone()]) }, cx);
                }))
        });
        let ring = if ringing {
            gpui::Rgba { alpha: 0.45, ..theme::accent() }
        } else if speaking && seat.priority {
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
            .when(ringing, |tile| tile.border_dashed())
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
                if focused {
                    "Click to stop focusing"
                } else if room_context || !me {
                    "Click to focus. Right-click for more"
                } else {
                    "Click to focus"
                },
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
                .child(caption(if me { format!("{name} (You)") } else { name }, small, false, false))
        } else {
            base_tile
                .child(
                    Avatar::new(&people.key(&seat.id), &name, avatar)
                        .image_at(people.picture(&seat.id))
                        .build()
                        .when(ringing, |avatar| avatar.opacity(0.72)),
                )
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
                .when(ringing, |tile| {
                    tile.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(chip("Ringing…", theme::accent_text(), theme::accent_soft()).rounded_full())
                            .children(stop_ring),
                    )
                })
                .when(!small && !ringing, |tile| tile.children(state_row))
        };
        let has_link = link.is_some();
        tile.children(link)
            .children(seat.reaction.as_ref().filter(|r| !r.is_empty()).map(|emoji| {
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(if has_link { 36. } else { 8. }))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded_full()
                    .bg(theme::overlay())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(14.))
                    .child(emoji.clone())
            }))
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .flex()
                    .gap(px(4.))
                    .children(glyphs.into_iter().map(|(icon, color, _)| glyph(icon, 14., color))),
            )
            .when(!small && !ringing, |tile| tile.child(actions))
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
                } else if connecting {
                    this.call_leave(cx);
                } else {
                    this.telegram_leave_or_end(cx);
                }
            }))
            .child(glyph("icons/phone-off.svg", 18., theme::on_accent()))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::on_accent()).child(leave_word))
            .into_any_element();
        let has_call = call.is_some();
        let menu = self.call_menu();
        let input_open = matches!(menu, Some(Menu::Input { .. }));
        let output_open = matches!(menu, Some(Menu::Output { .. }));
        let camera_menu_open = matches!(menu, Some(Menu::Camera { .. } | Menu::CameraPreview));
        let mute_look = if opts.held.is_some() {
            BarLook::Held
        } else if muted {
            BarLook::Off
        } else {
            BarLook::Rest
        };
        let mute_icon = if opts.held.is_some() || muted { "icons/mic-off.svg" } else { "icons/mic.svg" };
        let mute_label: SharedString = match opts.held {
            Some(words) => words.into(),
            None if muted => "Unmute".into(),
            None => "Mute".into(),
        };
        let mute: Stateful<Div> = if has_call {
            bar_split_button(
                "tiny-bar-mute-pill",
                "tiny-bar-mute",
                "tiny-bar-mute-menu",
                mute_icon,
                mute_look,
                input_open,
                mute_label,
                opts.held.is_none().then_some("⌘⇧M"),
                "Select microphone",
                opts.held.is_none().then(|| -> Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)> {
                    Box::new(cx.listener(move |this, _, _, cx| {
                        if room_context {
                            this.tiny_toggle_mute(cx);
                        } else {
                            this.call_mute(cx);
                        }
                    }))
                }),
                Box::new(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Input { from_hub: false }, cx);
                })),
                Box::new(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Input { from_hub: false }, cx);
                })),
            )
        } else {
            match opts.held {
                Some(words) => bar_button("tiny-bar-mute", "icons/mic-off.svg", BarLook::Held)
                    .aria_label(words)
                    .tooltip(tip(words, None)),
                None => bar_button("tiny-bar-mute", mute_icon, mute_look)
                    .aria_label(mute_label.clone())
                    .tooltip(tip(mute_label, Some("⌘⇧M")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if room_context {
                            this.tiny_toggle_mute(cx);
                        } else {
                            this.call_mute(cx);
                        }
                    })),
            }
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
        let deafen_look = if opts.held_ears {
            BarLook::Held
        } else if deafened {
            BarLook::Off
        } else {
            BarLook::Rest
        };
        let deafen_icon = if opts.held_ears || deafened {
            "icons/headphone-off.svg"
        } else {
            "icons/headphones.svg"
        };
        let deafen_label: SharedString = if opts.held_ears {
            "A moderator deafened you here".into()
        } else if deafened {
            "Undeafen".into()
        } else {
            "Deafen".into()
        };
        let deafen: Stateful<Div> = if has_call {
            bar_split_button(
                "tiny-bar-deafen-pill",
                "tiny-bar-deafen",
                "tiny-bar-deafen-menu",
                deafen_icon,
                deafen_look,
                output_open,
                deafen_label,
                (!opts.held_ears).then_some("⌘⇧D"),
                "Select speaker",
                (!opts.held_ears).then(|| -> Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)> {
                    Box::new(cx.listener(move |this, _, _, cx| {
                        if room_context {
                            this.tiny_toggle_deafen(cx);
                        } else {
                            this.call_deafen(cx);
                        }
                    }))
                }),
                Box::new(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Output { from_hub: false }, cx);
                })),
                Box::new(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Output { from_hub: false }, cx);
                })),
            )
        } else if opts.held_ears {
            bar_button("tiny-bar-deafen", "icons/headphone-off.svg", BarLook::Held)
                .aria_label("A moderator deafened you here")
                .tooltip(tip("A moderator deafened you here", None))
        } else {
            bar_button("tiny-bar-deafen", deafen_icon, deafen_look)
                .aria_label(deafen_label.clone())
                .tooltip(tip(deafen_label, Some("⌘⇧D")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if room_context {
                        this.tiny_toggle_deafen(cx);
                    } else {
                        this.call_deafen(cx);
                    }
                }))
        };
        let joined = call.is_some_and(|call| call.state == CallState::Joined);
        let is_rtmp = call.is_some_and(|call| call.rtmp_stream);
        let pictures = joined && opts.video_allowed && !is_rtmp;
        let share_on = call.is_some_and(|call| sending(call, Content::Share));
        let camera_on = call.is_some_and(|call| sending(call, Content::Camera));
        let share_limit = call.and_then(|c| c.video_limit_reason(share_on));
        let share = pictures.then(|| {
            if let Some(reason) = share_limit {
                bar_button("tiny-bar-share", "icons/monitor-up.svg", BarLook::Held)
                    .aria_label(reason.clone())
                    .tooltip(tip(reason, None))
            } else {
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
            }
        });
        let camera_limit = call.and_then(|c| c.video_limit_reason(camera_on));
        let camera = pictures.then(|| {
            let call_cam = call.cloned();
            let has_cameras = call_cam.as_ref().is_some_and(|c| !c.cameras.is_empty());
            if let Some(reason) = camera_limit {
                bar_button("tiny-bar-camera", "icons/video.svg", BarLook::Held)
                    .aria_label(reason.clone())
                    .tooltip(tip(reason, None))
            } else if let Some(call_for_split) = call_cam.filter(|_| has_cameras) {
                let call_click = call_for_split.clone();
                let call_menu = call_for_split.clone();
                let call_right = call_for_split;
                let cam_label: SharedString =
                    if camera_on { "Turn camera off" } else { "Turn camera on" }.into();
                bar_split_button(
                    "tiny-bar-camera-pill",
                    "tiny-bar-camera",
                    "tiny-bar-camera-menu",
                    "icons/video.svg",
                    if camera_on { BarLook::On } else { BarLook::Rest },
                    camera_menu_open,
                    cam_label,
                    None,
                    "Select camera",
                    Some(Box::new(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_camera_clicked(&call_click, camera_on, cx);
                    }))),
                    Box::new(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_camera_picker_toggle(&call_menu, camera_on, cx);
                    })),
                    Box::new(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.call_camera_picker_toggle(&call_right, camera_on, cx);
                    })),
                )
            } else {
                let call_click = call.cloned();
                bar_button(
                    "tiny-bar-camera",
                    "icons/video.svg",
                    if camera_on { BarLook::On } else { BarLook::Rest },
                )
                .aria_label(if camera_on { "Turn camera off" } else { "Turn camera on" })
                .tooltip(tip(if camera_on { "Turn camera off" } else { "Turn camera on" }, None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(call) = &call_click {
                        this.call_camera_clicked(call, camera_on, cx);
                    } else {
                        this.call_ask(Request::Media(MediaRequest::Camera { on: !camera_on }), cx);
                    }
                }))
            }
        });
        let popout_open = self.call_popout_open();
        let popout = pictures.then(|| {
            bar_button("tiny-bar-popout", "icons/picture-in-picture-2.svg", if popout_open { BarLook::On } else { BarLook::Rest })
                .aria_label(if popout_open { "Bring video back" } else { "Pop out video" })
                .tooltip(tip(if popout_open { "Bring video back" } else { "Pop out video" }, Some("⇧⌘O")))
                .on_click(cx.listener(|this, _, _, cx| this.call_toggle_popout(cx)))
        });
        let signal_group = joined
            && call.is_some_and(|c| c.conversation.network == Network::Signal && c.conversation.id.starts_with("g."));
        let hand_raised = call.is_some_and(|c| c.hand_raised);
        let raise_hand = signal_group.then(|| {
            bar_button(
                "tiny-bar-hand",
                "icons/hand.svg",
                if hand_raised { BarLook::On } else { BarLook::Rest },
            )
            .aria_label(if hand_raised { "Lower hand" } else { "Raise hand" })
            .tooltip(tip(if hand_raised { "Lower hand" } else { "Raise hand" }, None))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.call_ask(Request::CallRaiseHand { raised: !hand_raised }, cx);
            }))
        });
        let react_bar = signal_group.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .px(px(4.))
                .h(px(36.))
                .rounded_full()
                .bg(theme::selected())
                .children(["👍", "❤️", "😂", "👏"].into_iter().enumerate().map(|(ix, emoji)| {
                    div()
                        .id(("call-react", ix))
                        .size(px(28.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(14.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(tip(format!("React with {emoji}"), None))
                        .child(emoji)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.call_ask(Request::CallReact { emoji: emoji.to_owned() }, cx);
                        }))
                }))
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
        let devices = bar_button(
            "tiny-bar-devices",
            "icons/settings.svg",
            if menu == Some(Menu::Devices) { BarLook::On } else { BarLook::Rest },
        )
        .aria_label("Call options")
        .tooltip(tip("Call options", None))
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
        let bar_popover = call.and_then(|call| {
            let open = menu.as_ref()?;
            if let Some(body) = self.call_device_popover_body(call, open, cx) {
                return Some(body);
            }
            match open {
                Menu::AddPeople => Some((280., self.call_add_people_menu(call, cx).p(px(6.)))),
                Menu::Soundboard => Some((320., self.call_soundboard_body(call, cx).p(px(8.)))),
                Menu::Person { account, user_id, name } => {
                    Some((260., self.call_person_menu(account.clone(), user_id, name, cx).p(px(6.))))
                }
                _ => None,
            }
        });
        let device_notice_click = self.call_refused().is_none()
            && call.and_then(|c| c.device_notice.as_deref()).is_some();
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
                    .children(raise_hand)
                    .children(react_bar)
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
                let target = super::devices::stuck_device_menu(&detail);
                div()
                    .id("tiny-bar-notice")
                    .max_w(px(520.))
                    .whitespace_normal()
                    .type_small()
                    .text_color(theme::warning())
                    .when(device_notice_click, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.underline())
                            .tooltip(tip("Click to switch device", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.call_menu_toggle(target.clone(), cx);
                            }))
                    })
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
                            .size(px(22.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::pressed()))
                            .tooltip(tip(if starred { "Remove from favorites" } else { "Favorite sound" }, None))
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
        let focused_stream = match &self.call_state.focus {
            Some(Focus::Stream(uid)) => Some(uid.clone()),
            _ => None,
        };
        if let Some(uid) = focused_stream {
            self.call_stop_watching_user(&uid, window, cx);
            return;
        }
        self.call_unwatch(cx);
        self.call_sync_watch_now(cx);
    }

    /// Stops watching one specific stream. On Discord this removes only `user_id` from the
    /// watched Go Live streams so every other watched stream and all cameras stay live.
    pub(in crate::gui) fn call_stop_watching_user(&mut self, user_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.call_keep_keys(window, cx);
        if self.call_state.focus.as_ref() == Some(&Focus::Stream(user_id.to_owned())) {
            self.call_state.focus = None;
        }
        if self.call_now().is_some_and(|call| call.conversation.network == Network::Discord) {
            self.call_state.discord_watched_shares.retain(|id| id != user_id);
            if self.call_state.watch_asked.as_ref().is_some_and(|(id, _)| id == user_id) {
                self.call_state.watch_asked = None;
                self.call_state.watch_timer = None;
            }
            self.call_sync_watch_now(cx);
            cx.notify();
            return;
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

    /// Steps Escape back out of the call view: first clear stage focus (or exit 1:1 auto-video to
    /// the duo grid), then leave the call view.
    pub(in crate::gui) fn call_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let auto_video_call = self.snapshot.as_ref().and_then(|snapshot| {
            let call = self.call_shown(snapshot)?;
            let subject = Subject::of(self, snapshot, &call);
            let row = self.call_conversation_row(snapshot, &call);
            let peer = row.as_ref().and_then(|row| row.peer.as_deref());
            let (me_id, me_name) = self.call_me(snapshot, &call);
            let stage = Stage::of_call(&call, &subject, &me_id, &me_name, peer);
            let streams = stage::streams(&stage, &me_id, sending_now(Some(&call)));
            self.call_peer_video(&stage, &streams, Some(&call))
                .map(|_| call.conversation.clone())
        });
        if self.call_state.focus.take().is_some() {
            if let Some(key) = auto_video_call {
                self.call_state.grid_choice_for = Some(key);
            }
            self.call_sync_watch_now(cx);
            cx.notify();
            return true;
        }
        if let Some(key) = auto_video_call {
            if self.call_state.view.as_ref() == Some(&key)
                && self.call_state.grid_choice_for.as_ref() != Some(&key)
            {
                self.call_state.grid_choice_for = Some(key);
                cx.notify();
                return true;
            }
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

#[cfg(test)]
mod tests {
    use super::stage_tile_size;

    #[test]
    fn stage_tiles_scale_with_width_and_seat_count() {
        for (width, count, expected) in [
            (360., 1, (312., 234.)),
            (360., 2, (260., 195.)),
            (360., 3, (164., 126.)),
            (464., 1, (320., 240.)),
            (464., 2, (202., 152.)),
            (464., 3, (202., 152.)),
            (464., 4, (202., 152.)),
            (464., 6, (202., 152.)),
            (900., 1, (320., 240.)),
            (900., 2, (280., 210.)),
            (900., 4, (248., 186.)),
            (900., 6, (210., 158.)),
        ] {
            assert_eq!(stage_tile_size(width, count, false), expected, "{width}px, {count} seats");
        }
    }

    #[test]
    fn a_pair_fits_on_one_row_with_padding_at_and_above_the_duo_threshold() {
        for width in [396., 464., 900.] {
            let (tile, _) = stage_tile_size(width, 2, false);
            assert!(2. * tile + 12. + 48. <= width);
        }
        let (tile, _) = stage_tile_size(395., 2, false);
        assert!(2. * tile + 12. + 48. > 395.);
    }

    #[test]
    fn stage_tiles_stay_compact_when_streams_are_visible() {
        for (width, count) in [(360., 1), (464., 4), (900., 6)] {
            assert_eq!(stage_tile_size(width, count, true), (150., 96.));
        }
    }
}
