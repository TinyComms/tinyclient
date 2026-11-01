//! The stage of the phone's call screen: one person large (a voice call, a ring), one picture
//! full-bleed (a video call with one person), a grid that adapts to how many are in the call,
//! or a screen share with everyone in a strip under it. My own camera, when others have
//! pictures, is a tile in a corner that a finger can move to another corner.
//!
//! Pictures: the room forwards one picture (`Call::picture`, 06 §Pictures), so one person's
//! tile shows their camera and anyone else whose camera is on offers Watch. My own camera has
//! no picture of its own here until the platform hands one over (`self_frame`).

use super::{first, in_call, sending, Corner, Drag, Shell, HEADER};
use crate::gui::call::Subject;
use crate::gui::components::{glyph, tint_of, Avatar};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::theme;
use gpui::{div, img, prelude::*, px, AnyElement, Context, Div, MouseButton, ObjectFit, SharedString};
use protocol::{Call, CallState, Content, ConversationKind, Key, MediaRequest, PictureKind, Request};
use std::path::PathBuf;

const GAP: f32 = 8.;
const RADIUS: f32 = 18.;
/// My camera's corner tile.
const PIP_W: f32 = 100.;
const PIP_H: f32 = 144.;
const PIP_INSET: f32 = 16.;
/// A person in the strip under a screen share.
const STRIP_W: f32 = 88.;
const STRIP_H: f32 = 112.;

/// How the stage is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Layout {
    /// One face large: a voice call with one person, or a call that is not up yet.
    Hero,
    /// One person's picture fills the stage.
    Solo,
    /// Everyone as tiles.
    Grid,
    /// A screen share on the stage, everyone in a strip under it.
    Share,
}

/// One person on the stage.
struct Tile {
    id: String,
    name: String,
    key: Key,
    face: Option<PathBuf>,
    speaking: bool,
    muted: bool,
    deafened: bool,
    camera: bool,
    sharing: bool,
    me: bool,
    /// Their camera's picture, when it is the one on screen.
    frame: Option<PathBuf>,
}

/// How many columns and rows `n` tiles take on a phone held upright.
pub(super) fn grid(n: usize) -> (usize, usize) {
    match n {
        0 | 1 => (1, 1),
        2 => (1, 2),
        3 | 4 => (2, 2),
        5 | 6 => (2, 3),
        7 | 8 => (2, 4),
        n => (3, n.div_ceil(3)),
    }
}

/// A picture filling its box, cut to fit or shown whole.
fn picture(path: PathBuf, radius: f32, whole: bool) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .rounded(px(radius))
        .overflow_hidden()
        .child(img(path).size_full().rounded(px(radius)).object_fit(if whole { ObjectFit::Contain } else { ObjectFit::Cover }))
}

/// A dark chip over a picture or a tile: a name, a state.
fn chip() -> Div {
    div()
        .flex_none()
        .h(px(26.))
        .px(px(8.))
        .rounded(px(9.))
        .bg(gpui::rgba(0x0000008c))
        .flex()
        .items_center()
        .gap(px(5.))
}

/// The person's color, faint, as a tile's ground: each tile reads as its person.
fn wash(key: &Key) -> gpui::Hsla {
    let (fill, _) = theme::tint(tint_of(key));
    gpui::Hsla { alpha: 0.22, ..fill }
}

/// A large face in its speaking ring: a 3 pt ring that glows while they speak.
pub(super) fn halo(avatar: Div, size: f32, speaking: bool) -> Div {
    let ring = if speaking { theme::positive() } else { gpui::Rgba { alpha: 0., ..theme::positive() } };
    div().relative().size(px(size)).flex_none().child(avatar).child(
        div()
            .absolute()
            .top(px(-7.))
            .left(px(-7.))
            .size(px(size + 14.))
            .rounded_full()
            .border_3()
            .border_color(ring)
            .when(speaking, |ring| {
                ring.shadow(vec![gpui::BoxShadow {
                    color: gpui::Rgba { alpha: 0.35, ..theme::positive() }.into(),
                    offset: gpui::point(px(0.), px(0.)),
                    blur_radius: px(18.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            }),
    )
}

impl Shell {
    /// Who is on the stage: everyone else, then me.
    fn call_phone_tiles(&self, call: &Call, subject: &Subject) -> Vec<Tile> {
        let mut tiles: Vec<Tile> = call
            .participants
            .iter()
            .map(|who| {
                let key = Key { id: who.id.clone(), ..call.conversation.clone() };
                Tile {
                    id: who.id.clone(),
                    name: who.name.clone(),
                    face: self.call_phone_face(call, subject, &who.id),
                    key,
                    speaking: who.speaking && !who.server_muted && !who.suppressed,
                    muted: who.muted || who.server_muted,
                    deafened: who.deafened || who.server_deafened,
                    camera: who.camera,
                    sharing: who.sharing,
                    me: false,
                    frame: self.call_phone_frame(call, &who.id, PictureKind::Camera),
                }
            })
            .collect();
        let (name, face) = self.call_phone_me();
        let camera = sending(call, Content::Camera);
        tiles.push(Tile {
            id: String::new(),
            key: Key { id: "me".into(), ..call.conversation.clone() },
            name,
            face,
            speaking: call.speaking && !call.muted && !call.deafened,
            muted: call.muted || call.deafened,
            deafened: call.deafened,
            camera,
            sharing: sending(call, Content::Share),
            me: true,
            frame: camera.then(|| self.call_phone_self_frame()).flatten(),
        });
        tiles
    }

    /// The picture of `id`'s camera or screen, when it is the one on screen.
    fn call_phone_frame(&self, call: &Call, id: &str, kind: PictureKind) -> Option<PathBuf> {
        #[cfg(feature = "gui-shot")]
        if let Some(frame) = crate::gui::call::shot::phone_frame(id, kind == PictureKind::Share) {
            return Some(frame);
        }
        let picture = call.picture.as_ref().filter(|picture| !picture.path.is_empty() && picture.user_id == id && picture.kind == kind)?;
        Some(PathBuf::from(&picture.path))
    }

    /// My own camera's picture, as the platform last wrote it (`hooks::self_frame`); without
    /// one the tile says the camera is on instead.
    fn call_phone_self_frame(&self) -> Option<PathBuf> {
        #[cfg(feature = "gui-shot")]
        if let Some(frame) = crate::gui::call::shot::phone_frame("", false) {
            return Some(frame);
        }
        // A platform's own camera first; the demo has none, so its own call shows my portrait.
        if let Some(frame) = super::hooks::self_frame() {
            return Some(frame);
        }
        #[cfg(any(feature = "gui-shot", feature = "demo"))]
        if crate::gui::shot_demo::on() {
            return crate::gui::shot_demo::live::self_frame().map(PathBuf::from);
        }
        None
    }

    /// How the stage is laid out for this call now.
    pub(super) fn call_phone_layout(&self, call: &Call, subject: &Subject) -> Layout {
        if !in_call(call) {
            return Layout::Hero;
        }
        let sharer = call.participants.iter().find(|who| who.sharing);
        if sharer.is_some_and(|who| self.call_phone_frame(call, &who.id, PictureKind::Share).is_some()) {
            return Layout::Share;
        }
        let direct = subject.kind == ConversationKind::Direct && call.participants.len() <= 1;
        if !direct {
            return Layout::Grid;
        }
        match call.participants.first() {
            Some(peer) if self.call_phone_frame(call, &peer.id, PictureKind::Camera).is_some() => Layout::Solo,
            _ => Layout::Hero,
        }
    }

    /// My camera sits in a corner instead of in the grid: it is on, and someone else's picture
    /// or face is the stage.
    fn call_phone_pip_shown(&self, call: &Call, layout: Layout) -> bool {
        if !sending(call, Content::Camera) || !in_call(call) {
            return false;
        }
        match layout {
            Layout::Hero | Layout::Solo => true,
            Layout::Share => false,
            Layout::Grid => call.participants.iter().any(|who| self.call_phone_frame(call, &who.id, PictureKind::Camera).is_some()),
        }
    }

    pub(super) fn call_phone_stage(&self, call: &Call, subject: &Subject, layout: Layout, wide: f32, tall: f32, cx: &mut Context<Self>) -> AnyElement {
        match layout {
            Layout::Hero => self.call_phone_hero(call, subject, cx).into_any_element(),
            Layout::Solo => {
                let tiles = self.call_phone_tiles(call, subject);
                match tiles.into_iter().find(|tile| !tile.me) {
                    Some(peer) => self.call_phone_tile(&peer, wide, tall, cx).into_any_element(),
                    None => self.call_phone_hero(call, subject, cx).into_any_element(),
                }
            }
            Layout::Grid => self.call_phone_grid(call, subject, layout, wide, tall, cx),
            Layout::Share => self.call_phone_share(call, subject, wide, tall, cx).into_any_element(),
        }
    }

    /// One face large in the middle: who is rung, who it is with, a voice channel's plate.
    fn call_phone_hero(&self, call: &Call, subject: &Subject, cx: &mut Context<Self>) -> Div {
        let peer = call.participants.first().filter(|_| subject.kind == ConversationKind::Direct);
        let live = in_call(call);
        let speaking = live && peer.is_some_and(|peer| peer.speaking);
        let face = peer.and_then(|peer| self.call_phone_face(call, subject, &peer.id));
        let size = 128.;
        let lead = self.call_phone_lead(&call.conversation, subject, face, size, false);
        let peer_muted = live && peer.is_some_and(|peer| peer.muted);
        let lead = halo(lead, size, speaking).when(peer_muted, |lead| {
            lead.child(
                div()
                    .absolute()
                    .bottom(px(-2.))
                    .right(px(-2.))
                    .size(px(36.))
                    .rounded_full()
                    .bg(theme::canvas())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(30.)).rounded_full().bg(theme::danger_soft()).flex().items_center().justify_center().child(glyph("icons/mic-off.svg", 16., theme::danger()))),
            )
        });
        let alone = self.call_left_alone(call);
        let line: Option<(String, gpui::Rgba)> = match (&call.state, peer) {
            (CallState::RingingOut, _) => Some(("Ringing…".into(), theme::text_soft())),
            (CallState::Connecting, _) => Some(("Connecting…".into(), theme::text_soft())),
            (CallState::Reconnecting, _) => Some(("Reconnecting…".into(), theme::warning())),
            _ if alone.is_some() => alone.map(|name| (format!("{} left the call", first(&name)), theme::text_soft())),
            (_, Some(peer)) if peer.muted => Some((format!("{} is muted", first(&peer.name)), theme::text_soft())),
            (_, Some(peer)) if peer.speaking => Some(("Speaking".into(), theme::positive())),
            (_, Some(_)) => None,
            _ if subject.kind != ConversationKind::Direct => Some((
                match call.participants.len() {
                    0 => "Only you are here".to_owned(),
                    1 => "1 other person".to_owned(),
                    n => format!("{n} other people"),
                },
                theme::text_soft(),
            )),
            _ => None,
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(20.))
            .pb(px(24.))
            .child(lead)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .px(px(24.))
                    .child(
                        div()
                            .max_w_full()
                            .text_center()
                            .whitespace_normal()
                            .line_clamp(2)
                            .text_size(px(tokens::scaled(26.)))
                            .line_height(px(tokens::scaled(32.)))
                            .font_weight(theme::SEMIBOLD)
                            .child(subject.title.clone()),
                    )
                    .children(line.map(|(words, color)| {
                        // A ring and a reconnect count how long, beside what they are.
                        let counting = matches!(call.state, CallState::RingingOut | CallState::Reconnecting);
                        let clock = counting.then(|| self.call_phone_clock(call, cx)).flatten();
                        div().flex().gap(px(8.)).phone_row().text_color(color).child(words).children(clock)
                    })),
            )
    }

    /// Everyone as tiles, in as many columns and rows as their number asks for. Past eight the
    /// grid scrolls.
    fn call_phone_grid(&self, call: &Call, subject: &Subject, layout: Layout, wide: f32, tall: f32, cx: &mut Context<Self>) -> AnyElement {
        let pip = self.call_phone_pip_shown(call, layout);
        let tiles: Vec<Tile> = self.call_phone_tiles(call, subject).into_iter().filter(|tile| !(pip && tile.me)).collect();
        let (cols, rows) = grid(tiles.len());
        let tile_w = (wide - (cols as f32 - 1.) * GAP) / cols as f32;
        let fit = (tall - (rows as f32 - 1.) * GAP) / rows as f32;
        let tile_h = if fit < 100. { (tile_w * 1.1).round() } else { fit };
        let mut column = div().id("call-phone-grid").size_full().overflow_y_scroll().flex().flex_col().gap(px(GAP));
        for row in tiles.chunks(cols) {
            column = column.child(
                div()
                    .flex_none()
                    .w_full()
                    .flex()
                    .justify_center()
                    .gap(px(GAP))
                    .children(row.iter().map(|tile| self.call_phone_tile(tile, tile_w, tile_h, cx))),
            );
        }
        column.into_any_element()
    }

    /// A person's tile: their picture, or their face on their color; their name and their mic
    /// at the bottom; what else they are doing at the top. The speaking ring is always laid
    /// out and only lit, so a tile that starts speaking moves nothing.
    fn call_phone_tile(&self, tile: &Tile, wide: f32, tall: f32, cx: &mut Context<Self>) -> Div {
        let small = wide.min(tall);
        let avatar = (small * 0.42).clamp(40., 120.).round();
        let ring = if tile.speaking { theme::positive() } else { gpui::Rgba { alpha: 0., ..theme::positive() } };
        // A narrow tile (the strip under a share) names the person by their first name.
        let narrow = wide < 140.;
        let name = match (tile.me, narrow) {
            (true, _) => "You".to_owned(),
            (false, true) => first(&tile.name).to_owned(),
            (false, false) => tile.name.clone(),
        };
        let avatar = if narrow { avatar.min(44.) } else { avatar };
        let label = chip()
            .max_w(px(wide - 20.))
            .children(tile.muted.then(|| glyph("icons/mic-off.svg", 13., theme::danger())))
            .child(div().min_w_0().truncate().text_size(px(tokens::scaled(13.))).font_weight(theme::MEDIUM).text_color(gpui::rgb(0xffffff)).child(name));
        let marks = div()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .flex()
            .gap(px(4.))
            .children(tile.deafened.then(|| chip().px(px(6.)).child(glyph("icons/headphone-off.svg", 13., theme::text_soft()))))
            .children(tile.sharing.then(|| chip().px(px(6.)).child(glyph("icons/monitor-up.svg", 13., theme::accent_text()))));
        // Their camera is on and someone else's picture is the one on screen: Watch theirs.
        let watch = (tile.camera && tile.frame.is_none() && !tile.me).then(|| {
            let source = tile.id.clone();
            div()
                .id(SharedString::from(format!("call-phone-watch-{}", tile.id)))
                .h(px(30.))
                .px(px(12.))
                .rounded_full()
                .bg(theme::overlay())
                .border_1()
                .border_color(theme::overlay_edge())
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .active(|style| style.bg(theme::pressed()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.call_ask(Request::Media(MediaRequest::ShareWatch { source: Some(source.clone()) }), cx)))
                .child(glyph("icons/video.svg", 14., theme::text()))
                .child(div().phone_stamp().child("Watch"))
        });
        let body = match &tile.frame {
            Some(frame) => div().relative().size_full().child(picture(frame.clone(), RADIUS - 2., false)),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                // Keep the face clear of the name at the bottom of a short tile.
                .when(tall < 160., |body| body.pb(px(30.)))
                .child(super::super::ringed(Avatar::new(&tile.key, &tile.name, avatar).image_at(tile.face.clone()).build(), avatar, false))
                .children(watch),
        };
        div()
            .relative()
            .flex_none()
            .w(px(wide))
            .h(px(tall))
            .rounded(px(RADIUS))
            .bg(theme::raised())
            .child(div().absolute().top_0().left_0().size_full().rounded(px(RADIUS)).bg(wash(&tile.key)))
            .child(div().absolute().top_0().left_0().size_full().p(px(2.)).child(div().relative().size_full().child(body)))
            .child(div().absolute().left(px(10.)).bottom(px(10.)).child(label))
            .child(marks)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(RADIUS))
                    .border_2()
                    .border_color(ring)
                    .when(!tile.speaking, |edge| edge.border_1().border_color(theme::hairline())),
            )
    }

    /// Someone's screen on the stage, whole, on black; everyone in a strip under it.
    fn call_phone_share(&self, call: &Call, subject: &Subject, wide: f32, tall: f32, cx: &mut Context<Self>) -> Div {
        let tiles = self.call_phone_tiles(call, subject);
        let sharer = tiles.iter().find(|tile| tile.sharing && !tile.me);
        let frame = sharer.and_then(|tile| self.call_phone_frame(call, &tile.id, PictureKind::Share));
        let caption = sharer.map(|tile| format!("{} is sharing", first(&tile.name))).unwrap_or_else(|| "Screen share".into());
        let stage_h = (tall - STRIP_H - 12.).max(120.);
        let stop = div()
            .id("call-phone-stop-watching")
            .h(px(30.))
            .px(px(12.))
            .rounded_full()
            .bg(gpui::rgba(0x0000008c))
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .active(|style| style.bg(theme::pressed()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.call_ask(Request::Media(MediaRequest::ShareWatch { source: None }), cx)))
            .child(glyph("icons/x.svg", 13., theme::text()))
            .child(div().phone_stamp().text_color(theme::text()).child("Stop watching"));
        let screen = div()
            .relative()
            .flex_none()
            .w(px(wide))
            .h(px(stage_h))
            .rounded(px(RADIUS))
            .bg(gpui::rgb(0x000000))
            .border_1()
            .border_color(theme::hairline())
            .children(frame.map(|frame| picture(frame, RADIUS, true)))
            .child(
                div()
                    .absolute()
                    .top(px(10.))
                    .left(px(10.))
                    .right(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        chip()
                            .h(px(30.))
                            .px(px(10.))
                            .child(glyph("icons/monitor-up.svg", 14., theme::accent_text()))
                            .child(div().min_w_0().truncate().phone_stamp().text_color(gpui::rgb(0xffffff)).child(caption)),
                    )
                    .child(stop),
            );
        let strip = div()
            .id("call-phone-strip")
            .flex_none()
            .w(px(wide))
            .h(px(STRIP_H))
            .overflow_x_scroll()
            .flex()
            .gap(px(GAP))
            .children(tiles.iter().map(|tile| self.call_phone_tile(tile, STRIP_W, STRIP_H, cx)));
        div().size_full().flex().flex_col().gap(px(12.)).child(screen).child(strip)
    }

    /// Where my camera's tile sits: a corner, moved by the finger while it is dragged.
    fn call_phone_pip_at(&self, wide: f32, tall: f32, controls_h: f32) -> (f32, f32) {
        let top = tokens::safe_top() + HEADER + PIP_INSET;
        let bottom = tall - controls_h - PIP_INSET - PIP_H;
        let (left, right) = (PIP_INSET + 4., wide - PIP_INSET - 4. - PIP_W);
        let (x, y) = match self.call_state.phone.corner {
            Corner::TopLeft => (left, top),
            Corner::TopRight => (right, top),
            Corner::BottomLeft => (left, bottom),
            Corner::BottomRight => (right, bottom),
        };
        let by = self.call_state.phone.drag.map_or((0., 0.), |drag| drag.by);
        (x + by.0, y + by.1)
    }

    /// My camera, in a corner: its picture, or my face and "Camera on"; the camera's flip on it.
    pub(super) fn call_phone_pip(&self, call: &Call, layout: Layout, wide: f32, tall: f32, controls_h: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.call_phone_pip_shown(call, layout) {
            return None;
        }
        let (x, y) = self.call_phone_pip_at(wide, tall, controls_h);
        let (name, face) = self.call_phone_me();
        let key = Key { id: "me".into(), ..call.conversation.clone() };
        let body = match self.call_phone_self_frame() {
            Some(frame) => div().size_full().relative().child(picture(frame, 14., false)),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .bg(wash(&key))
                .child(Avatar::new(&key, &name, 48.).image_at(face).build())
                .child(div().phone_stamp().text_color(theme::text_soft()).child("Camera on")),
        };
        let front = !self.call_state.phone.back_camera;
        let flip = div()
            .id("call-phone-pip-flip")
            .absolute()
            .right(px(6.))
            .bottom(px(6.))
            .size(px(30.))
            .rounded_full()
            .bg(gpui::rgba(0x0000008c))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.call_state.phone.back_camera = front;
                super::hooks::flip_camera(!front);
                cx.notify();
            }))
            .child(glyph("icons/switch-camera.svg", 15., theme::text()));
        let size = (wide, tall, controls_h);
        Some(
            div()
                .id("call-phone-pip")
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(PIP_W))
                .h(px(PIP_H))
                .rounded(px(16.))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::border_strong())
                .shadow(theme::shadow_popover())
                .p(px(1.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.call_state.phone.drag = Some(Drag { from: crate::gui::phone::at(event.position), by: (0., 0.) });
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                    if let Some(drag) = this.call_state.phone.drag.as_mut().filter(|_| event.pressed_button == Some(MouseButton::Left)) {
                        let at = crate::gui::phone::at(event.position);
                        drag.by = (at.0 - drag.from.0, at.1 - drag.from.1);
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| this.call_phone_pip_drop(size, cx)),
                )
                .child(div().size_full().rounded(px(15.)).overflow_hidden().child(body))
                .child(flip)
                .into_any_element(),
        )
    }

    /// The finger lifted off my camera's tile: it goes to the corner nearest where it was let go.
    fn call_phone_pip_drop(&mut self, (wide, tall, controls_h): (f32, f32, f32), cx: &mut Context<Self>) {
        if self.call_state.phone.drag.is_none() {
            return;
        }
        let (x, y) = self.call_phone_pip_at(wide, tall, controls_h);
        let (cx_, cy) = (x + PIP_W / 2., y + PIP_H / 2.);
        let middle = (tokens::safe_top() + HEADER + tall - controls_h) / 2.;
        self.call_state.phone.corner = match (cx_ < wide / 2., cy < middle) {
            (true, true) => Corner::TopLeft,
            (false, true) => Corner::TopRight,
            (true, false) => Corner::BottomLeft,
            (false, false) => Corner::BottomRight,
        };
        self.call_state.phone.drag = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_adapts_to_how_many_are_in_the_call() {
        assert_eq!(grid(1), (1, 1));
        assert_eq!(grid(2), (1, 2), "two people stack");
        assert_eq!(grid(3), (2, 2));
        assert_eq!(grid(4), (2, 2));
        assert_eq!(grid(6), (2, 3));
        assert_eq!(grid(7), (2, 4));
        assert_eq!(grid(12), (3, 4), "past eight, three columns");
        for n in 1..30 {
            let (cols, rows) = grid(n);
            assert!(cols * rows >= n, "{n} fits");
        }
    }
}
