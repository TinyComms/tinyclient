//! Moving pictures in a message: videos, and GIFs that are really MP4s (Discord's `gifv` from
//! Tenor and Giphy, Telegram's animations). The one entry point is [`Pane::moving`]: given the
//! tile that already draws the still (the network's poster), it adds what says the picture moves
//! and what a click does with it.
//!
//! The shell links no video decoder. mediad's H.264 decode is annex-b for a call, not an MP4
//! demuxer, and a message does not open a call to play. An MP4 gifv stays this poster. A click
//! opens the viewer, which is the player: play, scrub, and volume move a [`Reel`], and one byte
//! window is read through the media cache. Nothing is handed to the system's player. One GIF at
//! a time, stopped when it scrolls away, waits on frames the shell cannot decode yet.
//!
//! A round video message plays in the circle: mediad demuxes the MP4, decodes H.264 and hands
//! each frame over as a JPEG. A click pauses, the ring is how far along it is, and the speed is
//! the voice player's. Muted autoplay follows "Play GIFs and animated stickers".

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
    SharedString, Stateful, Div,
};
use protocol::{round_click, round_should_autoplay, Attachment, Message, PlayState, Playback, Request, RoundClick, RoundPhase};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Instant;

/// Bytes read for one position. Not the file.
pub(super) const WINDOW: u64 = 256 * 1024;

/// Where the viewer says a moving picture is. The picture on screen stays the poster.
#[derive(Clone, Debug)]
pub(super) struct Reel {
    pub playing: bool,
    pub at_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub muted: bool,
    pub looping: bool,
    since: Option<Instant>,
}

impl Reel {
    /// A gifv: muted, looping. `playing` follows the autoplay setting.
    pub(super) fn gif(duration_ms: u64, playing: bool) -> Self {
        Self {
            playing,
            at_ms: 0,
            duration_ms,
            volume: 0.,
            muted: true,
            looping: true,
            since: playing.then(Instant::now),
        }
    }

    /// A video: sound on, paused until the viewer asks.
    pub(super) fn clip(duration_ms: u64) -> Self {
        Self { playing: false, at_ms: 0, duration_ms, volume: 1., muted: false, looping: false, since: None }
    }

    pub(super) fn toggle(&mut self) {
        self.playing = !self.playing;
        if self.playing {
            if self.duration_ms > 0 && self.at_ms >= self.duration_ms && !self.looping {
                self.at_ms = 0;
            }
            self.since = Some(Instant::now());
        } else {
            self.since = None;
        }
    }

    pub(super) fn scrub(&mut self, at_ms: u64) {
        self.at_ms = if self.duration_ms == 0 { 0 } else { at_ms.min(self.duration_ms) };
        self.since = self.playing.then(Instant::now);
    }

    pub(super) fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0., 1.);
        self.muted = self.volume == 0.;
    }

    /// Moves the play head by `dt_ms`. A loop wraps. A video stops at the end.
    pub(super) fn tick(&mut self, dt_ms: u64) {
        if !self.playing || self.duration_ms == 0 || dt_ms == 0 {
            return;
        }
        let next = self.at_ms.saturating_add(dt_ms);
        if next < self.duration_ms {
            self.at_ms = next;
            return;
        }
        if self.looping {
            self.at_ms = next % self.duration_ms;
        } else {
            self.at_ms = self.duration_ms;
            self.playing = false;
            self.since = None;
        }
    }
}

/// Applies the time since the last draw. True while it is still playing.
pub(super) fn advance(reel: &mut Reel) -> bool {
    let now = Instant::now();
    if !reel.playing {
        reel.since = None;
        return false;
    }
    if let Some(since) = reel.since {
        let dt = now.saturating_duration_since(since).as_millis() as u64;
        if dt > 0 {
            reel.tick(dt);
            if reel.playing {
                reel.since = Some(now);
            }
        }
    } else {
        reel.since = Some(now);
    }
    reel.playing
}

/// The byte window for `at_ms`. An unknown length asks for [`WINDOW`] from the start.
pub(super) fn byte_span(at_ms: u64, duration_ms: u64, bytes: u64) -> (u64, u64) {
    if bytes == 0 {
        return (0, WINDOW);
    }
    let start = if duration_ms == 0 { 0 } else { bytes.saturating_mul(at_ms.min(duration_ms)) / duration_ms };
    let start = start.min(bytes - 1);
    let end = start.saturating_add(WINDOW).min(bytes);
    (start, end)
}

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
        let shape = media::shape(attachment);
        let mut tile = match shape {
            Shape::Gif => {
                let words = if fetching { "Loading…" } else { "GIF" };
                tile.child(play_disc()).child(badge(words.to_owned()).left(px(6.)))
            }
            _ => {
                let words = (attachment.duration > 0).then(|| media::duration_label(attachment.duration));
                tile.child(play_disc()).children(words.map(|words| badge(words).right(px(6.))))
            }
        };
        if shape == Shape::Video {
            if let Some(tr) = attachment.transcript.as_ref().filter(|t| !t.is_empty()) {
                let captions_active = self.captions_on.contains(&attachment.file.id);
                let (cc_id, cc_vtt) = (attachment.file.id.clone(), tr.file.clone());
                let cc = div()
                    .id(SharedString::from(format!("cc-{}", attachment.file.id)))
                    .absolute()
                    .bottom(px(6.))
                    .left(px(6.))
                    .h(px(20.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .bg(if captions_active { theme::accent() } else { gpui::rgba(0x000000aa) })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .type_micro()
                    .font_weight(theme::SEMIBOLD)
                    .text_color(gpui::white())
                    .tooltip(tip(if captions_active { "Hide captions" } else { "Show captions" }, None))
                    .child("CC")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_captions(cc_id.clone(), cc_vtt.clone(), cx);
                    }));
                if captions_active {
                    let lines = self.transcript_lines(tr);
                    let caption = if !lines.is_empty() {
                        let at = self
                            .playback_of(&attachment.file.id, cx)
                            .0
                            .as_ref()
                            .map(|p| p.position(crate::gui::playing::now_ms()));
                        at.and_then(|ms| protocol::active_caption(lines, ms))
                            .or_else(|| lines.first())
                            .map(|l| l.text.clone())
                    } else if !tr.preview.is_empty() {
                        Some(tr.preview.clone())
                    } else if tr.file.is_some() {
                        Some("Loading captions…".to_owned())
                    } else {
                        None
                    };
                    if let Some(caption) = caption {
                        tile = tile.child(
                            div()
                                .absolute()
                                .bottom(px(30.))
                                .left(px(8.))
                                .right(px(8.))
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(4.))
                                .bg(gpui::rgba(0x000000bf))
                                .type_small()
                                .text_color(gpui::white())
                                .child(caption),
                        );
                    }
                }
                tile = tile.child(cc);
            }
        }
        let file_id = attachment.file.id.clone();
        tile.tooltip(tip("Play", None)).on_click(cx.listener(move |this, _, window, cx| {
            if let Some((id, index)) = this.place_of(&file_id) {
                this.open_viewer(&id, index, window, cx);
            }
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

    fn place_of(&self, file_id: &str) -> Option<(String, usize)> {
        let root = self.open.as_ref().and_then(|open| open.root.as_ref()).map(|(message, _)| message);
        root.into_iter().chain(self.window.messages.iter()).find_map(|message| {
            message.attachments.iter().position(|held| held.file.id == file_id).map(|index| (message.id.clone(), index))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gif_loops_muted_and_a_video_stops_with_sound() {
        let mut gif = Reel::gif(1_000, true);
        assert!(gif.playing && gif.muted && gif.looping && gif.volume == 0.);
        gif.tick(1_500);
        assert_eq!(gif.at_ms, 500);
        assert!(gif.playing);

        let mut held = Reel::gif(1_000, false);
        held.tick(400);
        assert_eq!(held.at_ms, 0);
        held.toggle();
        assert!(held.playing);
        held.toggle();
        assert!(!held.playing);

        let mut clip = Reel::clip(1_000);
        assert!(!clip.playing && !clip.muted && !clip.looping && clip.volume == 1.);
        clip.toggle();
        clip.tick(1_200);
        assert_eq!(clip.at_ms, 1_000);
        assert!(!clip.playing);
        clip.tick(50);
        assert_eq!(clip.at_ms, 1_000);

        let mut unknown = Reel::clip(0);
        unknown.toggle();
        unknown.tick(50);
        assert_eq!(unknown.at_ms, 0);
        unknown.scrub(40);
        assert_eq!(unknown.at_ms, 0);
    }

    #[test]
    fn scrub_and_volume_stay_inside_the_picture() {
        let mut clip = Reel::clip(10_000);
        clip.scrub(12_000);
        assert_eq!(clip.at_ms, 10_000);
        clip.scrub(2_500);
        assert_eq!(clip.at_ms, 2_500);
        clip.set_volume(2.);
        assert_eq!(clip.volume, 1.);
        assert!(!clip.muted);
        clip.set_volume(-1.);
        assert_eq!(clip.volume, 0.);
        assert!(clip.muted);
        clip.toggle();
        assert!(clip.playing);
        clip.toggle();
        clip.toggle();
        assert_eq!(clip.at_ms, 2_500);
    }

    #[test]
    fn a_byte_window_follows_the_play_head_and_stays_short() {
        assert_eq!(byte_span(0, 1_000, 0), (0, WINDOW));
        assert_eq!(byte_span(0, 10_000, 1_000_000), (0, WINDOW));
        assert_eq!(byte_span(5_000, 10_000, 1_000_000), (500_000, 500_000 + WINDOW));
        assert_eq!(byte_span(10_000, 10_000, 100), (99, 100));
        assert_eq!(byte_span(0, 0, 500), (0, 500));
        let (start, end) = byte_span(9_000, 10_000, WINDOW);
        assert!(end - start <= WINDOW);
        assert!(start < end);
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
