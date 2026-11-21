//! Moving pictures in a message: videos, and GIFs that are really MP4s (Discord's `gifv` from
//! Tenor and Giphy, Telegram's animations). The one entry point is [`Pane::moving`]: given the
//! tile that already draws the still (the network's poster), it adds what says the picture moves
//! and what a click does with it. Anything else that draws a moving picture calls it too, so
//! there is one place that decides how video plays.
//!
//! Today a click fetches the file and hands it to the system's player (QuickTime on macOS, the
//! default app elsewhere), which decodes it with the system's own codecs, sound and controls
//! included; the shell links no video decoder. An inline player that loops a GIF muted while it
//! is on screen (decoded at the tile's size, one at a time, stopped when it scrolls away or the
//! window is not key) replaces the body of `moving` and keeps its signature.

use super::draw::{badge, play_disc};
use super::media::{self, Shape};
use super::{Pane, Then};
use crate::client::ask;
use crate::gui::components::tip;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, Stateful};
use protocol::{Attachment, Message, Request};

impl Pane {
    /// A video or GIF tile: the play disc over its still, a badge ("GIF", or the length, or
    /// "Loading…" while the file is fetched), a circle for a round video message, an unplayed dot
    /// when `message.media_unread` is set on a round video, and a click that plays it.
    pub(super) fn moving(&mut self, message: &Message, attachment: &Attachment, tile: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
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
        let unplayed_dot = (attachment.round && message.media_unread).then(|| {
            div()
                .absolute()
                .bottom(px(8.))
                .left(px(8.))
                .size(px(8.))
                .rounded_full()
                .bg(theme::accent())
        });
        let tile = if attachment.round { tile.rounded_full() } else { tile };
        let tile = tile.children(unplayed_dot);
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
}
