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

use super::draw::{badge, play_disc};
use super::media::{self, Shape};
use super::Pane;
use crate::gui::components::tip;
use gpui::{prelude::*, px, Context, Div, Stateful};
use protocol::Attachment;
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

impl Pane {
    /// A video or GIF tile: the play disc over its still, a badge, and a click that opens the
    /// viewer. The still is the picture. The file is not downloaded here.
    pub(super) fn moving(&mut self, attachment: &Attachment, tile: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        let tile = match media::shape(attachment) {
            Shape::Gif => tile.child(play_disc()).child(badge("GIF".to_owned()).left(px(6.))),
            _ => {
                let words = (attachment.duration > 0).then(|| media::duration_label(attachment.duration));
                tile.child(play_disc()).children(words.map(|words| badge(words).right(px(6.))))
            }
        };
        let tile = if attachment.round { tile.rounded_full() } else { tile };
        let file_id = attachment.file.id.clone();
        tile.tooltip(tip("Play", None)).on_click(cx.listener(move |this, _, window, cx| {
            if let Some((id, index)) = this.place_of(&file_id) {
                this.open_viewer(&id, index, window, cx);
            }
        }))
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
