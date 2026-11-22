//! Moving pictures in a message: videos, and GIFs that are really MP4s (Discord's `gifv` from
//! Tenor and Giphy, Telegram's animations). The one entry point is [`Pane::moving`]: given the
//! tile that already draws the still (the network's poster), it adds what says the picture moves
//! and what a click does with it. Anything else that draws a moving picture calls it too, so
//! there is one place that decides how video plays.
//!
//! A round video message plays in the circle: mediad demuxes the MP4, decodes H.264 and hands
//! each frame over as a JPEG. A click pauses, the ring is how far along it is, and the speed is
//! the voice player's. Muted autoplay follows "Play GIFs and animated stickers". A file mediad
//! cannot decode still opens in the system's player, which is also what every other video does.

use super::draw::{badge, play_disc};
use super::images::Picture;
use super::media::{self, Shape};
use super::player;
use super::{Pane, Then};
use crate::client::ask;
use crate::gui::anim::{self, Autoplay};
use crate::gui::components::tip;
use crate::gui::playing::{self, Play};
use crate::gui::theme::{self, Type};
use gpui::{
    canvas, div, fill, img, point, prelude::*, px, size, Bounds, Context, ImageSource, MouseButton, ObjectFit, Pixels,
    Stateful, Div,
};
use protocol::{round_click, round_should_autoplay, Attachment, Message, PlayState, Playback, Request, RoundClick, RoundPhase};
use std::f32::consts::{FRAC_PI_2, TAU};

/// How large a round note's frame is decoded. The note itself is about 400 px.
const ROUND_EDGE: u32 = 400;

fn phase(playback: Option<&Playback>, fetch_failed: bool) -> RoundPhase {
    if fetch_failed {
        return RoundPhase::Failed;
    }
    let Some(playback) = playback else {
        return RoundPhase::Idle;
    };
    match &playback.state {
        PlayState::Failed { .. } => RoundPhase::Failed,
        PlayState::Paused => RoundPhase::Paused,
        PlayState::Ended => RoundPhase::Ended,
        PlayState::Loading | PlayState::Playing if playback.muted => RoundPhase::Muted,
        PlayState::Loading | PlayState::Playing => RoundPhase::Sound,
    }
}

impl Pane {
    /// A video or GIF tile: the play disc over its still, a badge ("GIF", or the length, or
    /// "Loading…" while the file is fetched), a circle for a round video message, and a click
    /// that plays it. A round note plays in the circle. Every other video opens in the system player.
    pub(super) fn moving(&mut self, message: &Message, attachment: &Attachment, tile: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        if attachment.round {
            // The dot stays on a round note that plays in its circle.
            let unplayed_dot = message.media_unread.then(|| div().absolute().bottom(px(8.)).left(px(8.)).size(px(8.)).rounded_full().bg(theme::accent()));
            return self.round_note(attachment, tile, cx).children(unplayed_dot);
        }
        let fetching = self.then.contains_key(&attachment.file.id);
        let tile = match media::shape(attachment) {
            Shape::Gif => {
                let words = if fetching { "Loading…" } else { "GIF" };
                tile.child(play_disc()).child(badge(words.to_owned()).left(px(6.)))
            }
            _ => {
                let words = match fetching {
                    true => Some("Loading…".to_owned()),
                    false => (attachment.duration > 0).then(|| media::duration_label(attachment.duration)),
                };
                tile.child(play_disc()).children(words.map(|words| badge(words).right(px(6.))))
            }
        };
        let attached = attachment.clone();
        let mark_id = (attachment.round && message.media_unread && !message.outgoing).then(|| message.id.clone());
        tile.tooltip(tip("Play in your video player", None))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(message_id) = &mark_id {
                    if let Some(conversation) = this.key().cloned() {
                        let request = Request::MarkPlayed { conversation, message_id: message_id.clone() };
                        cx.background_executor().spawn(async move { ask(&request) }).detach();
                    }
                }
                this.fetch_then(&attached, Then::Open, cx);
            }))
    }

    fn round_note(&mut self, attachment: &Attachment, mut tile: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let file_id = attachment.file.id.clone();
        self.round_mark = self.round_stamp;
        let account = self.key().map(protocol::Key::account);
        let (playback, rate) = self.shell.upgrade().map_or((None, 100), |shell| {
            let state = &shell.read(cx).playing_state;
            let playback = account.as_ref().and_then(|account| state.of(account, &file_id).cloned());
            let rate = playback.as_ref().map_or(state.rate(), |playback| playback.rate);
            (playback, rate)
        });
        let fetching = self.play_after.as_ref().is_some_and(|(wanted, ..)| *wanted == file_id) || self.then.contains_key(&file_id);
        let now = playing::now_ms();
        let shown = player::shown(playback.as_ref(), fetching, attachment.file.failed.as_deref(), attachment.duration, now);
        let here = phase(playback.as_ref(), attachment.file.failed.is_some());
        if let Some(picture) = playback.as_ref().and_then(|playback| playback.picture.as_ref()) {
            if self.round_frame.as_ref().is_some_and(|(file, path)| file == &file_id && path != &picture.path) {
                self.let_go.push((self.round_frame.as_ref().map(|(_, path)| path.clone()).unwrap_or_default(), ROUND_EDGE));
            }
            self.round_frame = Some((file_id.clone(), picture.path.clone()));
            if let Picture::Ready(image) = self.picture(&picture.path, ROUND_EDGE, None, cx) {
                tile = tile.child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .rounded_full()
                        .child(img(ImageSource::Render(image)).size_full().object_fit(ObjectFit::Cover)),
                );
            }
        }
        let fraction = shown.fraction();
        if matches!(here, RoundPhase::Sound | RoundPhase::Muted | RoundPhase::Paused) {
            tile = tile.child(progress_ring(fraction));
        }
        let active = matches!(here, RoundPhase::Sound | RoundPhase::Muted);
        let badge_words = match (fetching, here) {
            (true, _) => Some("Loading…".to_owned()),
            (_, RoundPhase::Failed) => Some("Open".to_owned()),
            (_, RoundPhase::Sound | RoundPhase::Muted | RoundPhase::Paused) => Some(playing::clock(shown.at)),
            _ => (attachment.duration > 0).then(|| media::duration_label(attachment.duration)),
        };
        if !active {
            tile = tile.child(play_disc());
        }
        tile = tile.children(badge_words.map(|words| badge(words).right(px(6.))));
        if matches!(here, RoundPhase::Sound | RoundPhase::Muted | RoundPhase::Paused | RoundPhase::Ended) {
            let speed_file = file_id.clone();
            tile = tile.child(
                div()
                    .id("round-speed")
                    .absolute()
                    .bottom(px(6.))
                    .right(px(6.))
                    .px(px(5.))
                    .h(px(18.))
                    .rounded_full()
                    .bg(theme::raised())
                    .type_small()
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .child(playing::rate_label(rate))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let Some(account) = this.key().map(protocol::Key::account) else {
                            return;
                        };
                        let file = speed_file.clone();
                        let _ = this.shell.update(cx, |shell, cx| shell.play_faster(&account, &file, cx));
                    })),
            );
        }
        let setting = anim::autoplay();
        let hovered = self.round_hover.as_deref() == Some(file_id.as_str());
        if self.round_auto.as_deref() != Some(file_id.as_str())
            && round_should_autoplay(setting == Autoplay::Always, setting == Autoplay::Hover, hovered, crate::gui::app::reduce_motion(), self.active, here)
        {
            if let Some(play) = self.round_request(&file_id, true, Some(0)) {
                self.round_auto = Some(file_id.clone());
                let shell = self.shell.clone();
                cx.defer(move |cx| drop(shell.update(cx, |shell, cx| shell.play(play, cx))));
            }
        }
        let attached = attachment.clone();
        let tip_words = match here {
            RoundPhase::Failed => "Play in your video player",
            RoundPhase::Sound | RoundPhase::Muted => "Pause",
            _ => "Play",
        };
        let hover_id = file_id.clone();
        tile.rounded_full()
            .overflow_hidden()
            .tooltip(tip(tip_words, None))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let next = if *hovered { Some(hover_id.clone()) } else { None };
                if this.round_hover != next {
                    this.round_hover = next;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                match round_click(here) {
                    RoundClick::System => this.fetch_then(&attached, Then::Open, cx),
                    RoundClick::Pause => {
                        let _ = this.shell.update(cx, |shell, cx| shell.play_pause(cx));
                    }
                    RoundClick::Sound { at } => {
                        if let Some(play) = this.round_request(&attached.file.id, false, at) {
                            let _ = this.shell.update(cx, |shell, cx| shell.play(play, cx));
                            return;
                        }
                        let Some(message) = this.message_for_file(&attached.file.id) else {
                            return;
                        };
                        let index = message.attachments.iter().position(|item| item.file.id == attached.file.id).unwrap_or(0);
                        this.round_after = true;
                        this.play_after = Some((attached.file.id.clone(), message.id.clone(), index, at));
                        this.fetch(vec![attached.file.id.clone()], None, attached.file.failed.is_some(), cx);
                    }
                }
            }))
    }

    fn message_for_file(&self, file: &str) -> Option<std::rc::Rc<protocol::Message>> {
        self.items.iter().find_map(|item| {
            let message = item.message()?;
            message.attachments.iter().any(|attachment| attachment.file.id == file).then(|| std::rc::Rc::clone(message))
        })
    }

    /// What to ask chatd, once the round note is on disk.
    fn round_request(&self, file: &str, muted: bool, at: Option<u32>) -> Option<Play> {
        let message = self.message_for_file(file)?;
        let attachment = message.attachments.iter().find(|attachment| attachment.file.id == file)?;
        let key = self.key()?;
        let path = attachment.file.path.clone().or_else(|| self.known.get(file).cloned())?;
        let title = if message.sender.name.is_empty() { "Video message".to_owned() } else { message.sender.name.clone() };
        Some(Play {
            account: key.account(),
            file: file.to_owned(),
            path,
            conversation: key.clone(),
            message: message.id.clone(),
            title,
            at,
            round: true,
            muted,
            looped: true,
        })
    }
}

/// Dots around the circle, lit for `fraction` of the way from the top.
fn progress_ring(fraction: f32) -> impl IntoElement {
    let fraction = fraction.clamp(0., 1.);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let steps = 48i32;
            let lit = (fraction * steps as f32).round() as i32;
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let center_x = f32::from(bounds.origin.x) + width / 2.;
            let center_y = f32::from(bounds.origin.y) + height / 2.;
            let radius = width.min(height) / 2. - 3.;
            if radius <= 0. {
                return;
            }
            for step in 0..steps {
                let angle = -FRAC_PI_2 + (step as f32 / steps as f32) * TAU;
                let color = if step < lit { theme::accent() } else { theme::hairline() };
                let at = point(px(center_x + radius * angle.cos() - 1.5), px(center_y + radius * angle.sin() - 1.5));
                window.paint_quad(fill(Bounds::new(at, size(px(3.), px(3.))), color));
            }
        },
    )
    .absolute()
    .inset_0()
}
