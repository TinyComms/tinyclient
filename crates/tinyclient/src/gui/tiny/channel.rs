//! One voice channel in the pane: its topic, how full it is, push-to-talk and bitrate in the top
//! bar, then the stage, one tile per person in the call, ringed while they speak and labelled
//! with what is true of them ("Server muted"). Click a tile to focus it, drawn large; hover it
//! for its volume. Right-click a tile for the person menu. Over the stage, while my call here is
//! connecting, reconnecting or has failed, a banner says so with the time and what to do. A
//! picture being watched sits above the stage. Under the stage, Join, or while I am in it the
//! call's own controls — mute, deafen, share, camera, and Leave — with "You're muted",
//! push-to-talk, and a watch strip above them. A channel nobody is in says so, with Join and
//! which devices it will use.

use super::{join_blocked, model, Float, Page};
use crate::gui::call::health::{self, Grade};
use crate::gui::components::{button_disabled, button_with_icon, glyph, tip, Avatar, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, img, prelude::*, px, AnyElement, Context, Div, MouseButton, MouseDownEvent, ObjectFit, SharedString, Stateful};
use protocol::{Call, CallPicture, CallState, Content, Direction, MediaRequest, PictureKind, Request, TinyRoom, TinySeat};

const TILE_W: f32 = 196.;
const TILE_H: f32 = 148.;
/// A focused tile: the one person drawn large.
const FOCUS_W: f32 = 420.;
const FOCUS_H: f32 = 300.;
/// Said under the call while echo cancellation is switched off in Settings → Voice.
const HEADPHONES: &str = "Echo cancellation is off: wear headphones, or others hear themselves.";

/// A chip of words: `Push to talk`, `96 kb/s`.
pub(super) fn chip(words: impl Into<SharedString>, color: gpui::Rgba, fill: gpui::Rgba) -> Div {
    div().flex_none().h(px(20.)).px(px(6.)).rounded(px(theme::R_CHIP)).bg(fill).flex().items_center().type_micro().text_color(color).child(words.into())
}

/// How the round button of the call bar looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Rest,
    /// My mic or audio is off.
    Off,
    /// The server holds it off: shown, not pressable.
    Held,
    /// Share or camera is on.
    On,
}

/// The round button of the call bar.
fn bar_button(id: &'static str, icon: &'static str, look: Look) -> Stateful<Div> {
    let (fill, color) = match look {
        Look::Rest => (theme::selected(), theme::text()),
        Look::Off | Look::Held => (theme::danger_soft(), theme::danger()),
        Look::On => (theme::accent_soft(), theme::accent()),
    };
    div()
        .id(id)
        .relative()
        .size(px(44.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(fill)
        .map(|button| match look {
            Look::Held => button.opacity(0.55),
            _ => button
                .cursor_pointer()
                .tab_index(0)
                .hover(|style| style.bg(theme::pressed()))
                .focus_visible(|style| style.shadow(theme::focus_ring())),
        })
        .child(glyph(icon, 20., color))
}

/// What the person whose seat this is can be heard as, in my call: my own seat with what my
/// call says right now (it knows before the server echoes it), anyone else's with mediad's
/// speaking edge when it is quicker than the server's.
pub(super) fn as_heard(seat: &TinySeat, me: bool, call: Option<&Call>) -> TinySeat {
    let mut seat = seat.clone();
    let Some(call) = call.filter(|call| call.state == CallState::Joined) else {
        return seat;
    };
    if me {
        seat.muted = call.muted || call.deafened;
        seat.deafened = call.deafened;
        seat.speaking = call.speaking;
    } else if call.participants.iter().any(|who| who.id == seat.user_id && who.speaking) {
        seat.speaking = true;
    }
    seat
}

/// Push to talk, as the pill over the call bar says it: the key to hold, or that it is held.
#[derive(Debug, PartialEq, Eq)]
enum Talk {
    /// Not push to talk: no pill.
    Open,
    Waiting(Option<String>),
    Talking,
    Priority,
}

fn talk(push: bool, held: Option<bool>, key: Option<String>) -> Talk {
    match (push, held) {
        (_, Some(true)) => Talk::Priority,
        (true, Some(false)) => Talk::Talking,
        (true, None) => Talk::Waiting(key),
        (false, _) => Talk::Open,
    }
}

/// Our own encode of this picture is on.
fn sending(call: &Call, content: Content) -> bool {
    call.video.iter().any(|stream| stream.direction == Direction::Encode && stream.content == content)
}

/// A decode of this person was asked for, or their picture is on screen.
fn watching(call: &Call, user_id: &str) -> bool {
    call.video.iter().any(|stream| stream.direction == Direction::Decode && stream.content == Content::Share && stream.source == user_id)
        || call.picture.as_ref().is_some_and(|picture| !picture.path.is_empty() && picture.user_id == user_id)
}

/// Who the watch strip offers: someone else's share, else someone else's camera. A room with
/// video turned off offers neither, and our own seat is never it.
struct Shown {
    user_id: String,
    share: bool,
}

fn pictured(room: &TinyRoom, me: &str) -> Option<Shown> {
    if !room.video_enabled {
        return None;
    }
    let others: Vec<&TinySeat> = room.participants.iter().filter(|seat| seat.user_id != me).collect();
    if let Some(seat) = others.iter().find(|seat| seat.sharing) {
        return Some(Shown { user_id: seat.user_id.clone(), share: true });
    }
    others.iter().find(|seat| seat.camera).map(|seat| Shown { user_id: seat.user_id.clone(), share: false })
}

impl Shell {
    /// What the top bar says of a channel after its name.
    pub(super) fn tiny_channel_facts(&self, room: &TinyRoom) -> Div {
        let mut facts = div().flex().items_center().gap(px(6.)).min_w_0();
        if !room.topic.trim().is_empty() {
            facts = facts.child(div().flex_none().w(px(1.)).h(px(16.)).bg(theme::hairline())).child(
                div().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(room.topic.clone()),
            );
        }
        facts = facts.child(chip(
            if room.user_limit > 0 { format!("{} of {}", room.participants.len(), room.user_limit) } else { format!("{} in call", room.participants.len()) },
            if model::full(room) { theme::warning() } else { theme::text_soft() },
            if model::full(room) { theme::warning_soft() } else { theme::hover() },
        ));
        if room.ptt_only {
            facts = facts.child(chip("Push to talk", theme::accent_text(), theme::accent_soft()));
        }
        if room.bitrate_kbps > 0 {
            facts = facts.child(chip(format!("{} kb/s", room.bitrate()), theme::text_soft(), theme::hover()));
        }
        facts
    }

    /// My call, when it is in this room.
    fn tiny_call_here(&self, room: &TinyRoom) -> Option<Call> {
        self.tiny_call().filter(|call| call.conversation.id == room.id)
    }

    /// The call header's security slot, in a channel's top bar while I am in its call.
    pub(super) fn tiny_channel_security(&self, room: &TinyRoom, cx: &mut Context<Self>) -> Option<AnyElement> {
        let call = self.tiny_call_here(room).filter(|call| call.state == CallState::Joined)?;
        self.call_security(&call, crate::gui::call::security::Place::TopBar, cx)
    }

    pub(super) fn tiny_channel(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let mine = page.mine == Some(room.id.as_str());
        let call = self.tiny_call_here(room);
        let banner = call.as_ref().and_then(|call| self.tiny_call_banner(room, call, cx));
        let stage: AnyElement = if room.participants.is_empty() {
            self.tiny_empty_stage(page, room, cx)
        } else {
            self.tiny_stage(page, room, call.as_ref(), cx)
        };
        let up = call.as_ref().is_some_and(|call| !matches!(call.state, CallState::Failed { .. }));
        let picture = call.as_ref().and_then(|call| self.tiny_picture(page, room, call, cx));
        let bar = if room.participants.is_empty() && !mine && !up { None } else { Some(self.tiny_call_bar(page, room, mine || up, call.as_ref(), cx)) };
        div().size_full().flex().flex_col().children(banner).children(picture).child(stage).children(bar).into_any_element()
    }

    /// Over the stage while my call here is not simply up: connecting or reconnecting with how
    /// long it has been, or what ended it, with Rejoin.
    fn tiny_call_banner(&self, room: &TinyRoom, call: &Call, cx: &mut Context<Self>) -> Option<Div> {
        let (icon, color, fill, title, line): (&'static str, gpui::Rgba, gpui::Rgba, String, Option<String>) = match &call.state {
            CallState::Connecting => ("icons/refresh-cw.svg", theme::accent_text(), theme::accent_wash(), format!("Connecting to {}…", room.name), None),
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
            let word = if call.state == CallState::Connecting { "Cancel" } else { "Leave" };
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
                    .child(div().flex_none().size(px(30.)).rounded_full().bg(theme::hover()).flex().items_center().justify_center().child(glyph(icon, 15., color)))
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
                                    .children(clock.map(|clock| div().flex_none().type_small().text_color(color).child(clock))),
                            )
                            .children(line.map(|line| div().whitespace_normal().type_small().text_color(theme::text_soft()).child(line))),
                    )
                    .children(action)
                    .children(dismiss),
            ),
        )
    }

    /// The one picture being watched: 16:9 above the stage, with who it is and Stop watching.
    /// My own share is never drawn here.
    fn tiny_picture(&self, page: &Page, room: &TinyRoom, call: &Call, cx: &mut Context<Self>) -> Option<Div> {
        let picture: &CallPicture = call.picture.as_ref().filter(|picture| !picture.path.is_empty() && picture.user_id != page.me)?;
        let name = model::name_by_id(page.community, &picture.user_id);
        let kind = match picture.kind {
            PictureKind::Share => "screen",
            PictureKind::Camera => "camera",
        };
        let caption = format!("{name}'s {kind} · {}", room.name);
        let frame = self.call_state.feed.shown().map(|frame| img(frame.clone()).size_full().object_fit(ObjectFit::Contain));
        Some(
            div().flex_none().px(px(20.)).pt(px(16.)).child(
                div()
                    .w_full()
                    .rounded(px(12.))
                    .bg(gpui::rgb(0x000000))
                    .border_1()
                    .border_color(theme::hairline())
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(div().w_full().aspect_ratio(16. / 9.).max_h(px(420.)).min_h(px(160.)).p(px(6.)).children(frame))
                    .child(
                        div()
                            .min_h(px(32.))
                            .px(px(10.))
                            .py(px(6.))
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .bg(theme::raised())
                            .child(Avatar::named(&name, 18.).build())
                            .child(div().flex_1().min_w(px(120.)).whitespace_normal().line_clamp(2).type_small().text_color(theme::text()).child(caption))
                            .child(
                                div()
                                    .id("tiny-picture-stop")
                                    .flex_none()
                                    .h(px(24.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .cursor_pointer()
                                    .tab_index(0)
                                    .hover(|style| style.bg(theme::hover()))
                                    .focus_visible(|style| style.shadow(theme::focus_ring()))
                                    .tooltip(tip("Stop watching", None))
                                    .on_click(cx.listener(|this, _, _, cx| this.call_ask(Request::Media(MediaRequest::ShareWatch { source: None }), cx)))
                                    .child(glyph("icons/x.svg", 12., theme::text_soft()))
                                    .child(div().type_micro().text_color(theme::text_soft()).child("Stop watching")),
                            ),
                    ),
            ),
        )
    }

    /// The strip over the controls: my share, with Stop, or someone else's picture, with Watch.
    fn tiny_watch(&self, page: &Page, room: &TinyRoom, call: &Call, cx: &mut Context<Self>) -> Option<Div> {
        let (words, action, ours, ask) = if sending(call, Content::Share) {
            ("You're sharing".to_owned(), "Stop", true, Request::Media(MediaRequest::ShareSend { on: false }))
        } else {
            let shown = pictured(room, page.me)?;
            let name = model::name_by_id(page.community, &shown.user_id);
            let words = if shown.share { format!("{name} is sharing") } else { format!("{name}'s camera is on") };
            let on = watching(call, &shown.user_id);
            let source = (!on).then_some(shown.user_id);
            (words, if on { "Stop watching" } else { "Watch" }, false, Request::Media(MediaRequest::ShareWatch { source }))
        };
        Some(
            div()
                .min_h(px(32.))
                .max_w_full()
                .px(px(12.))
                .py(px(4.))
                .rounded(px(16.))
                .bg(if ours { theme::accent_soft() } else { theme::overlay() })
                .border_1()
                .border_color(if ours { theme::accent() } else { theme::overlay_edge() })
                .flex()
                .flex_wrap()
                .justify_center()
                .items_center()
                .gap(px(10.))
                .child(div().whitespace_normal().line_clamp(2).type_small().text_color(theme::text()).child(words))
                .child(
                    div()
                        .id("tiny-watch")
                        .h(px(22.))
                        .px(px(10.))
                        .rounded_full()
                        .bg(theme::selected())
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .tab_index(0)
                        .hover(|style| style.bg(theme::pressed()))
                        .focus_visible(|style| style.shadow(theme::focus_ring()))
                        .on_click(cx.listener(move |this, _, _, cx| this.call_ask(ask.clone(), cx)))
                        .child(div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(action)),
                ),
        )
    }

    /// The tiles: wrapping and centred, or one person large over a row of the rest.
    fn tiny_stage(&self, page: &Page, room: &TinyRoom, call: Option<&Call>, cx: &mut Context<Self>) -> AnyElement {
        let focus = self.tiny_state.focus.as_deref().filter(|id| room.participants.iter().any(|seat| seat.user_id == *id));
        let base = div().id("tiny-stage").flex_1().min_h_0().overflow_y_scroll().p(px(24.));
        match focus {
            None => base
                .flex()
                .flex_wrap()
                .content_center()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .children(room.participants.iter().map(|seat| self.tiny_tile(page, seat, call, false, cx)))
                .into_any_element(),
            Some(focus) => {
                let big = room.participants.iter().find(|seat| seat.user_id == focus).map(|seat| self.tiny_tile(page, seat, call, true, cx));
                let rest = room.participants.iter().filter(|seat| seat.user_id != focus).map(|seat| self.tiny_tile(page, seat, call, false, cx));
                base.flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(16.))
                    .children(big)
                    .child(div().flex().flex_wrap().justify_center().gap(px(12.)).children(rest))
                    .into_any_element()
            }
        }
    }

    fn tiny_tile(&self, page: &Page, seat: &TinySeat, call: Option<&Call>, big: bool, cx: &mut Context<Self>) -> AnyElement {
        let me = seat.user_id == page.me;
        let seat = as_heard(seat, me, call);
        let seat = &seat;
        let name = model::name_by_id(page.community, &seat.user_id);
        let speaking = seat.speaking && !seat.server_muted && !seat.suppressed;
        let color = page.community.member(&seat.user_id).and_then(|member| model::color_of(page.community, member)).map(gpui::rgb).unwrap_or(theme::text());
        let states = model::seat_states(seat);
        let mut glyphs = super::column::seat_glyphs(seat);
        let (percent, local_mute) = self.tiny_state.gain(&seat.user_id);
        if local_mute && !me {
            glyphs.push(("icons/volume-x.svg", theme::warning(), "Muted for you"));
        }
        let focused = self.tiny_state.focus.as_deref() == Some(seat.user_id.as_str());
        let group: SharedString = format!("tiny-tile-{}", seat.user_id).into();
        let (w, h, avatar) = if big { (FOCUS_W, FOCUS_H, 112.) } else { (TILE_W, TILE_H, 64.) };
        let user_id = seat.user_id.clone();
        let focus_id = seat.user_id.clone();
        let menu_id = seat.user_id.clone();
        // My own link, in my own tile: what everyone else hears me through.
        let link = (me && call.is_some_and(|call| call.state == CallState::Joined)).then(|| self.tiny_health()).flatten().map(|health| {
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
                    hover_button(format!("tiny-tile-volume-{menu_id}"), if local_mute { "icons/volume-x.svg" } else { "icons/volume-2.svg" }, "Volume and more")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.tiny_open_float(Float::Person { user_id: menu_id.clone(), at: event.position }, cx);
                            }),
                        ),
                )
            })
            .child(hover_button(format!("tiny-tile-pin-{}", seat.user_id), if focused { "icons/x.svg" } else { "icons/pin.svg" }, if focused { "Stop focusing" } else { "Focus" }));
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
            .when(percent != 100 && !local_mute && !me, |row| row.child(chip(format!("{percent} %"), theme::text_soft(), theme::hover())));
        let ring = if speaking && seat.priority {
            theme::accent_text()
        } else if speaking {
            theme::positive()
        } else if focused {
            theme::border_strong()
        } else {
            theme::hairline()
        };
        div()
            .id(SharedString::from(format!("tiny-tile-{}", seat.user_id)))
            .group(group)
            .relative()
            .w(px(w))
            .h(px(h))
            .rounded(px(theme::R_CARD + 4.))
            .bg(theme::raised())
            .border_2()
            .border_color(ring)
            .when(speaking, |tile| tile.shadow(vec![gpui::BoxShadow {
                color: gpui::Hsla::from(if seat.priority { theme::accent_text() } else { theme::positive() }).opacity(0.25),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(12.),
                spread_radius: px(0.),
                inset: false,
            }]))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .tooltip(tip(if focused { "Click to stop focusing" } else { "Click to focus. Right-click for more" }, None))
            .on_click(cx.listener(move |this, _, _, cx| this.tiny_toggle_focus(&focus_id, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| this.tiny_open_float(Float::Person { user_id: user_id.clone(), at: event.position }, cx)),
            )
            .child(Avatar::new(&page.key(&seat.user_id), &name, avatar).image_at(page.picture(&seat.user_id)).build())
            .child(
                div()
                    .max_w(px(w - 24.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().truncate().map(|name| if big { name.type_title() } else { name.type_ui() }).font_weight(theme::SEMIBOLD).text_color(color).child(name))
                    .when(me, |row| row.child(chip("You", theme::text_soft(), theme::hover()))),
            )
            .child(state_row)
            .children(link)
            .child(div().absolute().top(px(8.)).right(px(8.)).flex().gap(px(4.)).children(glyphs.into_iter().map(|(icon, color, _)| glyph(icon, 14., color))))
            .child(actions)
            .into_any_element()
    }

    /// A click on a tile: that person large, or everyone alike again.
    fn tiny_toggle_focus(&mut self, user_id: &str, cx: &mut Context<Self>) {
        self.tiny_state.focus = if self.tiny_state.focus.as_deref() == Some(user_id) { None } else { Some(user_id.to_owned()) };
        cx.notify();
    }

    fn tiny_empty_stage(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let blocked = join_blocked(page.live, &page.community.media);
        let join: AnyElement = match blocked {
            _ if !page.community.allowed("community.calls.join") => div().into_any_element(),
            Some(_) => button_disabled("Join channel").h(px(32.)).into_any_element(),
            None if self.tiny_state.call_busy => button_disabled("Joining…").h(px(32.)).into_any_element(),
            None => {
                let room_id = room.id.clone();
                button_with_icon("tiny-join-empty", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(32.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            }
        };
        let devices = self.tiny_device_hint(page);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .px(px(24.))
            .child(div().size(px(56.)).rounded_full().bg(theme::raised()).flex().items_center().justify_center().child(glyph("icons/volume-2.svg", 24., theme::text_faint())))
            .child(div().type_title().text_color(theme::text()).child(format!("Nobody is in {} yet", room.name)))
            .child(div().max_w(px(380.)).text_center().whitespace_normal().type_ui().text_color(theme::text_soft()).child(blocked.map(str::to_owned).unwrap_or(devices)))
            .child(join)
            .children(self.tiny_state.call_error.clone().map(|detail| div().max_w(px(380.)).text_center().whitespace_normal().type_small().text_color(theme::danger()).child(detail)))
            .into_any_element()
    }

    /// Which microphone and speaker a join will use, as far as is known.
    fn tiny_device_hint(&self, page: &Page) -> String {
        if !page.audio && !page.community.media.is_empty() {
            return "This server has no audio turned on, so joining only takes a seat.".to_owned();
        }
        match self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()).filter(|call| !call.input.is_empty()) {
            Some(call) => format!("You'll join with {} and hear through {}.", call.input, if call.output.is_empty() { "your speaker" } else { &call.output }),
            None => "You'll join with your default microphone and speaker. Change them in Settings › Audio & video.".to_owned(),
        }
    }

    /// Over the call bar: "You're muted" when I seem to be talking into a muted microphone,
    /// a moderator's mute, and the push-to-talk key, lit while it is held.
    fn tiny_call_pills(&self, room: &TinyRoom, call: Option<&Call>, held_by_server: Option<&'static str>, cx: &mut Context<Self>) -> Option<Div> {
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
                    format!("You're muted · {} to talk", crate::gui::shortcuts::label("⌘⇧M")),
                    theme::on_accent(),
                    theme::danger_fill(),
                )
                .cursor_pointer()
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .tooltip(tip("Unmute", Some("⌘⇧M")))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_mute(cx)))
                .into_any_element(),
            ),
            None => None,
        };
        let values = crate::gui::app::prefs::values();
        let push = values.on(protocol::settings::PUSH_TO_TALK) || room.ptt_only;
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
        // The most serious thing Call health has to say, when it is serious.
        let trouble = self.tiny_health().and_then(|health| health.hints().into_iter().next()).filter(|hint| hint.level == health::Level::Bad).map(|hint| {
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
        Some(div().w_full().flex().flex_wrap().justify_center().items_center().gap(px(8.)).children(muted).children(ptt).children(trouble))
    }

    /// Under the stage: Join for a channel I am not in, else mute, deafen, devices, call
    /// health and Leave.
    fn tiny_call_bar(&self, page: &Page, room: &TinyRoom, mine: bool, call: Option<&Call>, cx: &mut Context<Self>) -> Div {
        let bar = div().w_full().flex_none().pb(px(20.)).pt(px(8.)).flex().flex_col().items_center().gap(px(8.));
        if !mine {
            let blocked = join_blocked(page.live, &page.community.media);
            let join: AnyElement = if !page.community.allowed("community.calls.join") {
                div().type_ui().text_color(theme::text_soft()).child("You can't join calls on this server.").into_any_element()
            } else if model::full(room) {
                button_disabled(format!("{} is full · {} of {}", room.name, room.participants.len(), room.user_limit)).h(px(36.)).into_any_element()
            } else if blocked.is_some() || self.tiny_state.call_busy {
                button_disabled(if self.tiny_state.call_busy { "Joining…" } else { "Join channel" }).h(px(36.)).into_any_element()
            } else {
                let room_id = room.id.clone();
                button_with_icon("tiny-join", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(36.))
                    .px(px(18.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            };
            let under = if model::full(room) { "Someone has to leave before you can join.".to_owned() } else { blocked.map(str::to_owned).unwrap_or_else(|| self.tiny_device_hint(page)) };
            return bar
                .child(join)
                .child(div().type_small().text_color(theme::text_faint()).child(under))
                .children(self.tiny_state.call_error.clone().map(|detail| div().type_small().text_color(theme::danger()).child(detail)));
        }
        let (muted, deafened) = self.tiny_quiet();
        let my_seat = room.participants.iter().find(|seat| seat.user_id == page.me);
        // What the server holds over my microphone, which no button of mine undoes.
        let held = my_seat.and_then(|seat| {
            if seat.server_muted {
                Some("A moderator muted you here")
            } else if seat.suppressed {
                Some("You can't speak in this channel")
            } else {
                None
            }
        });
        let held_ears = my_seat.is_some_and(|seat| seat.server_deafened);
        let connecting = call.is_some_and(|call| matches!(call.state, CallState::Connecting | CallState::RingingOut));
        let speaking = call.is_some_and(|call| call.state == CallState::Joined && call.speaking) && !muted && held.is_none();
        let leave_word = if connecting { "Cancel" } else { "Leave" };
        let leave = if self.tiny_state.call_busy {
            button_disabled(leave_word).h(px(44.)).px(px(20.)).rounded_full().into_any_element()
        } else {
            div()
                .id("tiny-leave")
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
                .on_click(cx.listener(|this, _, _, cx| this.tiny_leave(cx)))
                .child(glyph("icons/phone-off.svg", 18., theme::on_accent()))
                .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::on_accent()).child(leave_word))
                .into_any_element()
        };
        let mute = match held {
            Some(words) => bar_button("tiny-bar-mute", "icons/mic-off.svg", Look::Held).tooltip(tip(words, None)),
            None => bar_button("tiny-bar-mute", if muted { "icons/mic-off.svg" } else { "icons/mic.svg" }, if muted { Look::Off } else { Look::Rest })
                .tooltip(tip(if muted { "Unmute" } else { "Mute" }, Some("⌘⇧M")))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_mute(cx))),
        }
        // While I am heard: a ring on the button, the same green as a speaking tile's.
        .when(speaking, |mute| mute.child(div().absolute().top_0().left_0().size_full().rounded_full().border_2().border_color(theme::positive())));
        let deafen = if held_ears {
            bar_button("tiny-bar-deafen", "icons/headphone-off.svg", Look::Held).tooltip(tip("A moderator deafened you here", None))
        } else {
            bar_button("tiny-bar-deafen", if deafened { "icons/headphone-off.svg" } else { "icons/headphones.svg" }, if deafened { Look::Off } else { Look::Rest })
                .tooltip(tip(if deafened { "Undeafen" } else { "Deafen" }, Some("⌘⇧D")))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_toggle_deafen(cx)))
        };
        let joined = call.is_some_and(|call| call.state == CallState::Joined);
        let pictures = joined && room.video_enabled;
        let share_on = call.is_some_and(|call| sending(call, Content::Share));
        let camera_on = call.is_some_and(|call| sending(call, Content::Camera));
        let share = pictures.then(|| {
            bar_button("tiny-bar-share", "icons/monitor-up.svg", if share_on { Look::On } else { Look::Rest })
                .tooltip(tip(if share_on { "Stop sharing" } else { "Share your screen" }, None))
                .on_click(cx.listener(move |this, _, _, cx| this.call_ask(Request::Media(MediaRequest::ShareSend { on: !share_on }), cx)))
        });
        let camera = pictures.then(|| {
            bar_button("tiny-bar-camera", "icons/video.svg", if camera_on { Look::On } else { Look::Rest })
                .tooltip(tip(if camera_on { "Turn camera off" } else { "Turn camera on" }, None))
                .on_click(cx.listener(move |this, _, _, cx| this.call_ask(Request::Media(MediaRequest::Camera { on: !camera_on }), cx)))
        });
        let watch = call.filter(|_| pictures).and_then(|call| self.tiny_watch(page, room, call, cx));
        let refused = self.call_refused().map(str::to_owned).or_else(|| call.and_then(|call| call.device_notice.clone()));
        let health = call.filter(|call| call.state == CallState::Joined).and_then(|_| self.tiny_health()).map(|health| {
            let grade = health.grade();
            let words = match health.rtt_ms {
                Some(ms) if grade != Grade::Unknown => format!("Call health: {} · {ms} ms", grade.words().to_lowercase()),
                _ => "Call health".to_owned(),
            };
            div()
                .id("tiny-bar-health")
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
        let no_sound = !page.audio && !page.community.media.is_empty();
        bar.children(self.tiny_call_pills(room, call, held, cx))
            .children(watch)
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
                    .child(
                        bar_button("tiny-bar-devices", "icons/settings.svg", Look::Rest)
                            .tooltip(tip("Microphone and speaker", None))
                            .on_click(|_, _, cx| crate::gui::settings::open(crate::gui::settings::Section::Audio, cx)),
                    )
                    .children(health)
                    .child(leave),
            )
            .when(no_sound, |bar| bar.child(div().type_small().text_color(theme::text_faint()).child("This server has no audio turned on, so you have a seat and no sound.")))
            // mediad cancels echo unless it was switched off (V10); only then does the call say to
            // wear headphones.
            .when(page.audio && !crate::gui::app::prefs::values().processing().echo_cancellation, |bar| bar.child(div().type_small().text_color(theme::text_faint()).child(HEADPHONES)))
            .children(refused.map(|detail| div().max_w(px(520.)).whitespace_normal().type_small().text_color(theme::warning()).child(detail)))
            .children(self.tiny_state.call_error.clone().map(|detail| div().type_small().text_color(theme::danger()).child(detail)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Key, Network, Participant};

    fn joined(muted: bool, deafened: bool, speaking: bool) -> Call {
        Call {
            conversation: Key { network: Network::Tiny, account: "night".into(), id: "lounge".into() },
            state: CallState::Joined,
            participants: vec![Participant { id: "mara".into(), name: "Mara".into(), speaking: true, ..Participant::default() }],
            muted,
            deafened,
            joined: 1,
            input: String::new(),
            output: String::new(),
            engine_linked: true,
            video: Vec::new(),
            speaking,
            level: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_level: None,
            input_id: String::new(),
            output_id: String::new(),
            device_notice: None,
            device_stuck: false,
            security: None,
            picture: None,
        }
    }

    fn seat(user_id: &str) -> TinySeat {
        TinySeat { user_id: user_id.into(), ..TinySeat::default() }
    }

    #[test]
    fn my_tile_shows_my_call_at_once_and_others_light_on_the_quicker_edge() {
        let mine = as_heard(&seat("robin"), true, Some(&joined(true, false, false)));
        assert!(mine.muted && !mine.speaking, "muted here before the server says so");
        let deaf = as_heard(&seat("robin"), true, Some(&joined(false, true, false)));
        assert!(deaf.muted && deaf.deafened, "deafened is muted too");
        assert!(as_heard(&seat("robin"), true, Some(&joined(false, false, true))).speaking);
        assert!(as_heard(&seat("mara"), false, Some(&joined(false, false, false))).speaking, "mediad heard Mara first");
        assert!(!as_heard(&seat("dev"), false, Some(&joined(false, false, false))).speaking);
        let connecting = Call { state: CallState::Connecting, ..joined(true, false, true) };
        assert_eq!(as_heard(&seat("robin"), true, Some(&connecting)), seat("robin"), "only a call that is up speaks for me");
    }

    #[test]
    fn a_share_is_offered_ahead_of_a_camera_and_never_our_own() {
        let room = TinyRoom {
            participants: vec![
                TinySeat { user_id: "me".into(), sharing: true, camera: true, ..TinySeat::default() },
                TinySeat { user_id: "ada".into(), camera: true, ..TinySeat::default() },
                TinySeat { user_id: "bev".into(), sharing: true, ..TinySeat::default() },
            ],
            ..TinyRoom::new("lounge", "Lounge")
        };
        let shown = pictured(&room, "me").expect("bev's share");
        assert_eq!(shown.user_id, "bev");
        assert!(shown.share);
        let cameras = TinyRoom {
            participants: vec![TinySeat { user_id: "ada".into(), camera: true, ..TinySeat::default() }],
            ..TinyRoom::new("lounge", "Lounge")
        };
        let shown = pictured(&cameras, "me").expect("ada's camera");
        assert_eq!((shown.user_id.as_str(), shown.share), ("ada", false));
        let quiet = TinyRoom { video_enabled: false, ..cameras };
        assert!(pictured(&quiet, "me").is_none(), "a room with video off offers nothing");
    }

    #[test]
    fn sending_is_our_encode_and_watching_is_that_person() {
        let mut call = joined(false, false, false);
        assert!(!sending(&call, Content::Share));
        call.video.push(protocol::VideoStream { direction: Direction::Encode, content: Content::Camera, source: String::new() });
        assert!(sending(&call, Content::Camera));
        assert!(!sending(&call, Content::Share));
        assert!(!watching(&call, "ada"));
        call.video.push(protocol::VideoStream { direction: Direction::Decode, content: Content::Share, source: "ada".into() });
        assert!(watching(&call, "ada"));
        assert!(!watching(&call, "bev"));
    }

    #[test]
    fn the_push_to_talk_pill_names_the_key_and_lights_while_it_is_held() {
        assert_eq!(talk(false, None, Some("F".into())), Talk::Open);
        assert_eq!(talk(true, None, Some("F".into())), Talk::Waiting(Some("F".into())));
        assert_eq!(talk(true, None, None), Talk::Waiting(None));
        assert_eq!(talk(true, Some(false), None), Talk::Talking);
        assert_eq!(talk(false, Some(true), None), Talk::Priority, "the priority key talks in either mode");
    }
}
