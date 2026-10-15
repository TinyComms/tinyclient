//! The voice message or audio file playing, wherever the user has gone since: what chatd said
//! about it (`Change::Playback`), the speed the next one starts at, the clock that moves the
//! players on screen while it plays, and the small now-playing bar above the call dock.
//!
//! Nothing here decodes or plays: chatd runs `mediad --play`. Its position moves with the clock
//! (`Playback::position`), so no tick crosses the socket; the shell redraws on a timer of its
//! own, and only while something plays.

use super::components::{glyph, tip};
use super::state::Shell;
use super::theme::{self, Type};
use crate::client::ask;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, SharedString, Window};
use protocol::{AccountKey, Key, PlayRequest, PlayState, Playback, Request, Response, RATES};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often a playing file's players are drawn again: a waveform bar of a short message
/// passes in about this long.
const TICK: Duration = Duration::from_millis(200);

/// What the user asked to play, and where it was posted.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub account: AccountKey,
    pub file: String,
    pub path: String,
    pub conversation: Key,
    pub message: String,
    pub title: String,
    pub at: Option<u32>,
}

#[derive(Default)]
pub struct State {
    /// What chatd last said. Ended files stay here until the next one, so their player shows
    /// the end.
    pub playback: Option<Playback>,
    /// The speed picked last, which the next file starts at. 0 before any pick: normal speed.
    rate: u16,
    /// What chatd refused, in its words, for the file it was about.
    pub refused: Option<(String, String)>,
    /// Where the file last played is on disk, to play it again from the now-playing bar.
    path: Option<String>,
    ticking: bool,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The speed after `rate` in the order a speed button steps through.
pub fn next_rate(rate: u16) -> u16 {
    let at = RATES.iter().position(|known| *known == rate).unwrap_or(0);
    RATES[(at + 1) % RATES.len()]
}

/// `1×`, `1.5×`, `2×`.
pub fn rate_label(rate: u16) -> String {
    match rate % 100 {
        0 => format!("{}×", rate / 100),
        rest => format!("{}.{}×", rate / 100, rest / 10),
    }
}

/// `0:07`, from milliseconds.
pub fn clock(ms: u32) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl State {
    pub fn rate(&self) -> u16 {
        if self.rate == 0 {
            100
        } else {
            self.rate
        }
    }

    /// The playback of this file, if it is the one chatd is playing or last played.
    pub fn of(&self, account: &AccountKey, file: &str) -> Option<&Playback> {
        self.playback.as_ref().filter(|playback| playback.is(account, file))
    }
}

impl Shell {
    /// `Change::Playback`. A file that plays starts the clock the players are drawn by.
    pub(super) fn playback_changed(&mut self, playback: Option<Playback>, cx: &mut Context<Self>) {
        let playing = playback.as_ref().is_some_and(|playback| playback.state == PlayState::Playing);
        self.playing_state.playback = playback;
        self.conversation_redraw(cx);
        cx.notify();
        if playing && !self.playing_state.ticking {
            self.playing_state.ticking = true;
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(TICK).await;
                let going = this.update(cx, |shell, cx| {
                    let playing = shell.playing_state.playback.as_ref().is_some_and(|playback| playback.state == PlayState::Playing);
                    if !playing {
                        shell.playing_state.ticking = false;
                        return false;
                    }
                    shell.conversation_redraw(cx);
                    cx.notify();
                    true
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            })
            .detach();
        }
    }

    fn play_ask(&mut self, request: PlayRequest, file: Option<String>, cx: &mut Context<Self>) {
        let request = Request::Play(request);
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words(&err)),
            };
            let _ = this.update(cx, |this, cx| {
                this.playing_state.refused = refused.map(|words| (file.unwrap_or_default(), words));
                cx.notify();
            });
        })
        .detach();
    }

    /// Plays a file that is on disk, from `at` or from where it was paused.
    pub(super) fn play(&mut self, play: Play, cx: &mut Context<Self>) {
        let rate = self.playing_state.of(&play.account, &play.file).map_or(self.playing_state.rate(), |shown| shown.rate);
        let file = play.file.clone();
        // Shown as starting at once; chatd's word replaces it.
        let at = play.at.unwrap_or_else(|| match self.playing_state.of(&play.account, &play.file) {
            Some(shown) if shown.state != PlayState::Ended => shown.at,
            _ => 0,
        });
        let duration = self.playing_state.of(&play.account, &play.file).map_or(0, |shown| shown.duration);
        self.playing_state.playback = Some(Playback {
            account: play.account.clone(),
            file: play.file.clone(),
            conversation: play.conversation.clone(),
            message: play.message.clone(),
            title: play.title.clone(),
            state: PlayState::Loading,
            at,
            since: now_ms(),
            duration,
            rate,
        });
        self.playing_state.refused = None;
        self.playing_state.path = Some(play.path.clone());
        let request = PlayRequest::Start {
            account: play.account,
            file: play.file,
            path: play.path,
            conversation: play.conversation,
            message: play.message,
            title: play.title,
            at: Some(at),
            rate,
            output: self.chosen_output(),
        };
        self.play_ask(request, Some(file), cx);
        self.conversation_redraw(cx);
        cx.notify();
    }

    pub(super) fn play_pause(&mut self, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.playing_state.playback {
            if matches!(shown.state, PlayState::Playing | PlayState::Loading) {
                shown.at = shown.position(now_ms());
                shown.state = PlayState::Paused;
            }
        }
        self.play_ask(PlayRequest::Pause, None, cx);
        self.conversation_redraw(cx);
        cx.notify();
    }

    /// Moves the file playing, or paused, to `at` milliseconds.
    pub(super) fn play_seek(&mut self, at: u32, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.playing_state.playback {
            shown.at = at;
            shown.since = now_ms();
            if shown.state == PlayState::Ended {
                shown.state = PlayState::Paused;
            }
        }
        self.play_ask(PlayRequest::Seek { at }, None, cx);
        self.conversation_redraw(cx);
        cx.notify();
    }

    /// Steps the speed: of the file playing when it is `file`, and of the next one either way.
    pub(super) fn play_faster(&mut self, account: &AccountKey, file: &str, cx: &mut Context<Self>) {
        let current = self.playing_state.of(account, file).map_or(self.playing_state.rate(), |shown| shown.rate);
        let rate = next_rate(current);
        self.playing_state.rate = rate;
        if let Some(shown) = self.playing_state.playback.as_mut().filter(|shown| shown.is(account, file)) {
            let now = now_ms();
            shown.at = shown.position(now);
            shown.since = now;
            shown.rate = rate;
            self.play_ask(PlayRequest::Rate { rate }, None, cx);
        }
        self.conversation_redraw(cx);
        cx.notify();
    }

    /// The now-playing bar's ✕: stops, and the bar goes.
    pub(super) fn play_stop(&mut self, cx: &mut Context<Self>) {
        self.playing_state.playback = None;
        self.play_ask(PlayRequest::Stop, None, cx);
        self.conversation_redraw(cx);
        cx.notify();
    }

    /// Above the call dock, in every sidebar mode: what is playing, or paused, with play and
    /// pause, the time, and a ✕. A click on the words goes back to the conversation.
    pub(super) fn now_playing(&self, cx: &mut Context<Self>) -> Option<Div> {
        let shown = self.playing_state.playback.as_ref()?;
        let failed = match &shown.state {
            PlayState::Ended => return None,
            PlayState::Failed { detail } => Some(detail.clone()),
            _ => None,
        };
        let playing = matches!(shown.state, PlayState::Playing | PlayState::Loading);
        let title = if shown.title.is_empty() { "Voice message".to_owned() } else { shown.title.clone() };
        let at = shown.position(now_ms());
        let time = match (&failed, shown.duration) {
            (Some(detail), _) => detail.clone(),
            (None, 0) => clock(at),
            // The length to the nearest second, as the message says it.
            (None, duration) => format!("{} / {}", clock(at), clock(duration.saturating_add(500))),
        };
        let (key, name) = (shown.conversation.clone(), title.clone());
        let button = div()
            .id("now-playing-toggle")
            .flex_none()
            .size(px(28.))
            .rounded_full()
            .bg(theme::accent_soft())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tooltip(tip(if playing { "Pause" } else { "Play" }, None))
            .child(super::conversation::player_glyph(playing, 12., theme::accent_text()))
            .on_click(cx.listener(|this, _, _, cx| {
                let Some(shown) = this.playing_state.playback.clone() else {
                    return;
                };
                if matches!(shown.state, PlayState::Playing | PlayState::Loading) {
                    this.play_pause(cx);
                } else {
                    this.play_again(cx);
                }
            }));
        let words = div()
            .id("now-playing-words")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .cursor_pointer()
            .child(div().truncate().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child(SharedString::from(title)))
            .child(
                div()
                    .truncate()
                    .type_micro()
                    .text_color(if failed.is_some() { theme::danger() } else { theme::text_soft() })
                    .child(time),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(key.clone(), &name, cx)));
        let close = div()
            .id("now-playing-stop")
            .flex_none()
            .size(px(24.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .tooltip(tip("Stop", None))
            .child(glyph("icons/x.svg", 14., theme::text_soft()))
            .on_click(cx.listener(|this, _, _, cx| this.play_stop(cx)));
        Some(
            div()
                .w_full()
                .flex_none()
                .px(px(8.))
                .pt(px(7.))
                .pb(px(8.))
                .border_t_1()
                .border_color(theme::hairline())
                .child(
                    div()
                        .h(px(40.))
                        .px(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::raised())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(button)
                        .child(words)
                        .child(close),
                ),
        )
    }

    /// Play on the now-playing bar of a paused or failed file: the file is still on disk.
    fn play_again(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.playing_state.playback.clone() else {
            return;
        };
        let Some(path) = self.playing_state.path.clone() else {
            return;
        };
        self.play(
            Play {
                account: shown.account,
                file: shown.file,
                path,
                conversation: shown.conversation,
                message: shown.message,
                title: shown.title,
                at: None,
            },
            cx,
        );
    }
}

/// For the sidebar's slot: the bar, when something is playing or paused.
pub(super) fn draw(shell: &Shell, _: &mut Window, cx: &mut Context<Shell>) -> Option<AnyElement> {
    shell.now_playing(cx).map(IntoElement::into_any_element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speed_button_steps_through_the_speeds_and_says_them_plainly() {
        assert_eq!(next_rate(100), 150);
        assert_eq!(next_rate(150), 200);
        assert_eq!(next_rate(200), 100);
        assert_eq!(next_rate(130), 150, "an unknown speed starts over");
        assert_eq!([100, 150, 200].map(rate_label), ["1×", "1.5×", "2×"]);
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(6_999), "0:06");
        assert_eq!(clock(83_000), "1:23");
    }
}
