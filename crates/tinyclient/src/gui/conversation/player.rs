//! A voice message's or audio file's player, in its message: play and pause, a waveform (the
//! network's own loudness, where it sent one) or a bar to scrub, the time, and the speed. The
//! file is fetched on the first press and plays when it lands; chatd plays it
//! (`crate::gui::playing`), so it goes on playing wherever the user goes.

use super::icons::{icon, Icon};
use super::media;
use super::tokens;
use super::Pane;
use crate::gui::components::tip;
use crate::gui::playing::{self, Play};
use crate::gui::theme::{self, Type};
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, Div, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Point, SharedString, Stateful,
};
use protocol::{Message, PlayState, Playback};
use std::rc::Rc;

/// Bars of a voice message's waveform.
const BARS: usize = 40;

/// A drag along a player's waveform or bar: where it would seek to, drawn until let go.
pub(super) struct Scrub {
    pub file: String,
    pub fraction: f32,
}

/// What a player shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    /// The file is on its way to disk.
    Fetching,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub status: Status,
    /// Milliseconds played, and in all.
    pub at: u32,
    pub duration: u32,
}

impl Shown {
    /// How far along, 0 to 1.
    pub fn fraction(&self) -> f32 {
        if self.duration == 0 {
            return 0.;
        }
        (self.at as f32 / self.duration as f32).clamp(0., 1.)
    }

    pub fn active(&self) -> bool {
        matches!(self.status, Status::Playing | Status::Loading)
    }

    /// `0:47` at rest; `0:14 / 0:47` once started.
    pub fn time(&self) -> String {
        match &self.status {
            Status::Idle => playing::clock(self.duration.saturating_add(500)),
            Status::Fetching => "Loading…".to_owned(),
            Status::Failed(_) => "Couldn't play · Retry".to_owned(),
            _ => format!("{} / {}", playing::clock(self.at), playing::clock(self.duration.saturating_add(500))),
        }
    }
}

/// A player's state from chatd's word on its file (`playback`, already matched to it), the
/// pane's own (fetching, or the fetch failed), and the length the network gave.
pub fn shown(playback: Option<&Playback>, fetching: bool, failed: Option<&str>, seconds: u32, now: u64) -> Shown {
    let mut duration = seconds * 1000;
    let Some(playback) = playback else {
        let status = match (fetching, failed) {
            (true, _) => Status::Fetching,
            (_, Some(reason)) => Status::Failed(reason.to_owned()),
            _ => Status::Idle,
        };
        return Shown { status, at: 0, duration };
    };
    if playback.duration > 0 {
        duration = playback.duration;
    }
    let status = match &playback.state {
        PlayState::Loading => Status::Loading,
        PlayState::Playing => Status::Playing,
        PlayState::Paused => Status::Paused,
        PlayState::Ended => Status::Ended,
        PlayState::Failed { detail } => Status::Failed(detail.clone()),
    };
    let at = match status {
        // The end is shown as the start again, ready to play once more.
        Status::Ended => 0,
        _ => playback.position(now).min(duration.max(playback.at)),
    };
    Shown { status, at, duration }
}

/// Where along `bounds` a point falls, 0 to 1.
pub fn along(bounds: Bounds<Pixels>, at: Point<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0. {
        return 0.;
    }
    (f32::from(at.x - bounds.origin.x) / width).clamp(0., 1.)
}

/// Play or pause, as one glyph.
pub(in crate::gui) fn glyph(playing: bool, size: f32, color: impl Into<Hsla>) -> gpui::Svg {
    icon(if playing { Icon::Pause } else { Icon::Play }, size, color)
}

impl Pane {
    fn playback_of(&self, file: &str, cx: &Context<Self>) -> (Option<Playback>, u16) {
        let account = self.key().map(protocol::Key::account);
        self.shell.upgrade().map_or((None, 100), |shell| {
            let state = &shell.read(cx).playing_state;
            let playback = account.and_then(|account| state.of(&account, file).cloned());
            let rate = playback.as_ref().map_or(state.rate(), |playback| playback.rate);
            (playback, rate)
        })
    }

    /// The play button: pauses what plays, else plays from where it was. A file not on disk is
    /// fetched first and plays when it lands.
    pub(super) fn press_play(&mut self, message: &Rc<Message>, index: usize, at: Option<u32>, cx: &mut Context<Self>) {
        let Some(attachment) = message.attachments.get(index) else {
            return;
        };
        let file = attachment.file.clone();
        let (playback, _) = self.playback_of(&file.id, cx);
        let active = playback.as_ref().is_some_and(|playback| matches!(playback.state, PlayState::Playing | PlayState::Loading));
        if active && at.is_none() {
            let _ = self.shell.update(cx, |shell, cx| shell.play_pause(cx));
            return;
        }
        if active || playback.is_some() {
            if let Some(at) = at {
                if active || playback.as_ref().is_some_and(|playback| playback.state == PlayState::Paused) {
                    let _ = self.shell.update(cx, |shell, cx| shell.play_seek(at, cx));
                    return;
                }
            }
        }
        let Some(play) = self.play_of(message, index, at) else {
            // Not on disk: fetched, then played. The user asked, so a failure is tried again.
            self.play_after = Some((file.id.clone(), message.id.clone(), index, at));
            self.fetch(vec![file.id.clone()], None, file.failed.is_some(), cx);
            cx.notify();
            return;
        };
        let _ = self.shell.update(cx, |shell, cx| shell.play(play, cx));
    }

    /// What to ask chatd to play, once the file is on disk.
    fn play_of(&self, message: &Message, index: usize, at: Option<u32>) -> Option<Play> {
        let attachment = message.attachments.get(index)?;
        let key = self.key()?;
        let path = attachment.file.path.clone().or_else(|| self.known.get(&attachment.file.id).cloned())?;
        let title = if attachment.waveform.is_empty() { attachment.name.clone() } else { message.sender.name.clone() };
        Some(Play {
            account: key.account(),
            file: attachment.file.id.clone(),
            path,
            conversation: key.clone(),
            message: message.id.clone(),
            title,
            at,
        })
    }

    /// A file asked for to be played has landed: it plays now.
    pub(super) fn play_landed(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some((wanted, message, index, at)) = self.play_after.clone() else {
            return;
        };
        if wanted != file {
            return;
        }
        self.play_after = None;
        let play = self.find(&message).and_then(|message| self.play_of(&message, index, at));
        if let Some(play) = play {
            // A landing is told through the shell, which is busy telling it: asked once it is done.
            let shell = self.shell.clone();
            cx.defer(move |cx| drop(shell.update(cx, |shell, cx| shell.play(play, cx))));
        }
    }

    fn faster(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(account) = self.key().map(protocol::Key::account) else {
            return;
        };
        let file = file.to_owned();
        let _ = self.shell.update(cx, |shell, cx| shell.play_faster(&account, &file, cx));
    }

    /// Let go of a drag: plays, or moves what plays, to where it was let go.
    fn scrubbed(&mut self, message: &Rc<Message>, index: usize, duration: u32, cx: &mut Context<Self>) {
        let Some(scrub) = self.scrub.take() else {
            return;
        };
        let at = (scrub.fraction * duration as f32) as u32;
        self.press_play(message, index, Some(at), cx);
        cx.notify();
    }

    /// The part a drag seeks along: the waveform or the bar. It records where it is drawn, so a
    /// press can be turned into a place in the file.
    fn scrubbable(&self, id: &str, message: &Rc<Message>, index: usize, duration: u32, cx: &mut Context<Self>) -> Stateful<Div> {
        let (bounds, file) = (self.scrub_bounds.clone(), id.to_owned());
        let mark = canvas(move |drawn, _, _| { bounds.borrow_mut().insert(file.clone(), drawn); }, |_, (), _, _| {});
        let (down, moved) = (id.to_owned(), id.to_owned());
        let (up, out) = ((message.clone(), index), (message.clone(), index));
        div()
            .id(SharedString::from(format!("scrub-{id}")))
            .relative()
            .cursor_pointer()
            .child(mark.absolute().top_0().left_0().size_full())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let Some(bounds) = this.scrub_bounds.borrow().get(&down).copied() else {
                        return;
                    };
                    this.scrub = Some(Scrub { file: down.clone(), fraction: along(bounds, event.position) });
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) || this.scrub.as_ref().is_none_or(|scrub| scrub.file != moved) {
                    return;
                }
                let Some(bounds) = this.scrub_bounds.borrow().get(&moved).copied() else {
                    return;
                };
                this.scrub = Some(Scrub { file: moved.clone(), fraction: along(bounds, event.position) });
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(move |this, _, _, cx| this.scrubbed(&up.0, up.1, duration, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(move |this, _, _, cx| this.scrubbed(&out.0, out.1, duration, cx)))
    }

    /// A voice message (`voice`, with its waveform) or an audio file (its name over a bar).
    pub(super) fn player_row(&mut self, message: &Rc<Message>, index: usize, voice: bool, cx: &mut Context<Self>) -> AnyElement {
        let attachment = &message.attachments[index];
        let file = attachment.file.clone();
        let (playback, rate) = self.playback_of(&file.id, cx);
        let fetching = self.play_after.as_ref().is_some_and(|(wanted, ..)| *wanted == file.id);
        let refused = self.shell.upgrade().and_then(|shell| {
            let state = &shell.read(cx).playing_state;
            state.refused.as_ref().filter(|(about, _)| *about == file.id).map(|(_, words)| words.clone())
        });
        let failed = refused.or_else(|| file.failed.clone());
        let mut shown = shown(playback.as_ref(), fetching, failed.as_deref(), attachment.duration, playing::now_ms());
        if let Some(scrub) = self.scrub.as_ref().filter(|scrub| scrub.file == file.id) {
            shown.at = (scrub.fraction * shown.duration as f32) as u32;
        }
        let reason = match &shown.status {
            Status::Failed(reason) => Some(reason.clone()),
            _ => None,
        };
        let duration = shown.duration;

        let (pressed, at) = (message.clone(), index);
        let button = div()
            .id(("play", index))
            .size(px(32.))
            .flex_none()
            .rounded_full()
            .bg(theme::accent_soft())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tooltip(tip(if shown.active() { "Pause" } else { "Play" }, None))
            .child(glyph(shown.active(), 14., theme::accent_text()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.press_play(&pressed, at, None, cx);
            }));
        let speed_file = file.id.clone();
        let speed = div()
            .id(("speed", index))
            .flex_none()
            .h(px(20.))
            .min_w(px(30.))
            .px(px(5.))
            .rounded(px(4.))
            .bg(theme::hover())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .type_micro()
            .font_weight(theme::SEMIBOLD)
            .text_color(if rate == 100 { theme::text_faint() } else { theme::text() })
            .tooltip(tip("Playback speed", None))
            .child(playing::rate_label(rate))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.faster(&speed_file, cx);
            }));
        let time = div()
            .flex_none()
            .type_small()
            .text_color(if reason.is_some() { theme::danger() } else { theme::text_soft() })
            .child(shown.time());
        let played = shown.fraction();

        let row = div()
            .id(("player", index))
            .w(px(360.))
            .max_w_full()
            .px(px(8.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(10.))
            .when_some(reason, |row, reason| row.tooltip(tip(format!("{reason} Click play to try again."), None)));
        let row = if voice {
            let bars = media::waveform(&attachment.waveform, BARS);
            let wave = self.scrubbable(&file.id, message, index, duration, cx).flex_1().min_w_0().h(px(28.)).flex().items_center().justify_between().overflow_hidden().children(
                bars.into_iter().enumerate().map(|(bar, tall)| {
                    let reached = (bar as f32 + 0.5) / BARS as f32 <= played && shown.status != Status::Idle;
                    div().w(px(2.)).h(px(tall)).flex_none().rounded(px(1.)).bg(if reached { theme::accent() } else { theme::text_faint() })
                }),
            );
            row.h(px(48.)).child(button).child(wave).child(time).child(speed)
        } else {
            let track = self.scrubbable(&file.id, message, index, duration, cx).flex_1().min_w_0().h(px(14.)).flex().items_center().child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(theme::hover())
                    .child(div().h_full().w(gpui::relative(played)).rounded(px(2.)).bg(theme::accent())),
            );
            let lines = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().truncate().type_ui().font_weight(theme::MEDIUM).child(attachment.name.clone()))
                .child(div().flex().items_center().gap(px(8.)).child(track).child(time));
            row.h(px(56.)).child(button).child(lines).child(speed)
        };
        row.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key, Network};

    fn playback(state: PlayState, at: u32) -> Playback {
        Playback {
            account: AccountKey { network: Network::Telegram, account: "1".into() },
            file: "d1".into(),
            conversation: Key { network: Network::Telegram, account: "1".into(), id: "2".into() },
            message: "3".into(),
            title: String::new(),
            state,
            at,
            since: 1_000,
            duration: 6_000,
            rate: 200,
        }
    }

    #[test]
    fn a_player_says_its_length_at_rest_and_where_it_is_once_started() {
        let idle = shown(None, false, None, 47, 0);
        assert_eq!((idle.status.clone(), idle.time(), idle.fraction()), (Status::Idle, "0:47".to_owned(), 0.));
        assert_eq!(shown(None, true, None, 47, 0).time(), "Loading…");
        let failed = shown(None, false, Some("The file is no longer there (404)."), 47, 0);
        assert_eq!(failed.time(), "Couldn't play · Retry");

        // chatd's length wins over the network's, and the position runs at the speed.
        let playing = shown(Some(&playback(PlayState::Playing, 1_000)), false, None, 5, 2_000);
        assert_eq!((playing.at, playing.duration), (3_000, 6_000));
        assert_eq!(playing.time(), "0:03 / 0:06");
        assert!(playing.active());
        assert!((playing.fraction() - 0.5).abs() < 1e-6);
        let paused = shown(Some(&playback(PlayState::Paused, 1_500)), false, None, 6, 99_000);
        assert_eq!((paused.at, paused.active()), (1_500, false));
        // The end goes back to the start, ready to play again.
        let ended = shown(Some(&playback(PlayState::Ended, 6_000)), false, None, 6, 99_000);
        assert_eq!((ended.status.clone(), ended.at), (Status::Ended, 0));
    }

    #[test]
    fn a_press_on_the_waveform_is_a_place_in_the_file() {
        let bounds = Bounds { origin: gpui::point(px(100.), px(0.)), size: gpui::size(px(200.), px(28.)) };
        assert_eq!(along(bounds, gpui::point(px(100.), px(5.))), 0.);
        assert_eq!(along(bounds, gpui::point(px(150.), px(5.))), 0.25);
        assert_eq!(along(bounds, gpui::point(px(400.), px(5.))), 1., "past the end is the end");
        assert_eq!(along(bounds, gpui::point(px(10.), px(5.))), 0.);
    }
}
