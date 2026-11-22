//! The typing row above the composer: who is typing, and three dots. The dots are the only
//! thing in the window that moves by itself, so they are their own entity: a 400 ms timer changes
//! one number and repaints this row alone. The timer exists only while someone is typing, the
//! window is the one in use, and for 30 seconds at most.

use crate::gui::people::Hub;
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{div, prelude::*, px, Context, Div, Window};
use protocol::{Compose, Key, Typist};
use std::time::{Duration, Instant};

/// The slot is always this tall, so messages never move when someone starts typing.
pub const HEIGHT: f32 = 20.;
const STEP: Duration = Duration::from_millis(400);
/// After this long of unbroken typing the dots hold still.
const MOVING: Duration = Duration::from_secs(30);

/// Present-participle phrasing for one activity: `(singular, plural, bare direct preview)`.
fn activity_verbs(activity: Compose) -> (&'static str, &'static str, &'static str) {
    match activity {
        Compose::Typing | Compose::Cancel => (" is typing…", " are typing…", "typing…"),
        Compose::RecordingVoice => (
            " is recording a voice message…",
            " are recording voice messages…",
            "recording a voice message…",
        ),
        Compose::RecordingVideo => (
            " is recording a video message…",
            " are recording video messages…",
            "recording a video message…",
        ),
        Compose::UploadingPhoto => (" is sending a photo…", " are sending photos…", "sending a photo…"),
        Compose::UploadingVideo => (" is sending a video…", " are sending videos…", "sending a video…"),
        Compose::UploadingAudio => (
            " is sending a voice message…",
            " are sending voice messages…",
            "sending a voice message…",
        ),
        Compose::UploadingFile => (" is sending a file…", " are sending files…", "sending a file…"),
        Compose::ChoosingSticker => (" is choosing a sticker…", " are choosing stickers…", "choosing a sticker…"),
        Compose::PlayingGame => (" is playing a game…", " are playing a game…", "playing a game…"),
        Compose::Speaking => (" is speaking…", " are speaking…", "speaking…"),
    }
}

/// What the row says for the active typists, carrying each person's activity verb, as
/// `(names, rest)` so the names can be set heavier.
pub fn wording_for(typists: &[Typist]) -> Option<(String, &'static str)> {
    let active: Vec<&Typist> = typists.iter().filter(|t| t.activity != Compose::Cancel).collect();
    match active.as_slice() {
        [] => None,
        [one] => Some((one.name.clone(), activity_verbs(one.activity).0)),
        [one, two] => {
            let verb = if one.activity == two.activity {
                activity_verbs(one.activity).1
            } else {
                " are typing…"
            };
            Some((format!("{} and {}", one.name, two.name), verb))
        }
        many => {
            let first = many[0].activity;
            let verb = if many.iter().all(|t| t.activity == first) {
                activity_verbs(first).1
            } else {
                " are typing…"
            };
            Some(("Several people".to_owned(), verb))
        }
    }
}

/// The sidebar's or header's preview for a conversation someone is active in: no name where there
/// is only one other person in a direct chat, and the right verb for recording, uploading, etc.
pub fn preview_for(typists: &[Typist], group: bool) -> Option<String> {
    let active: Vec<&Typist> = typists.iter().filter(|t| t.activity != Compose::Cancel).collect();
    match (active.as_slice(), group) {
        ([], _) => None,
        ([one], false) => Some(activity_verbs(one.activity).2.to_owned()),
        (_, false) => Some(activity_verbs(active[0].activity).2.to_owned()),
        (_, true) => wording_for(typists).map(|(names, rest)| format!("{names}{rest}")),
    }
}

/// What the row says for the people typing, as (names, rest), so the names can be set heavier.
#[cfg(test)]
pub fn wording(names: &[&str]) -> Option<(String, &'static str)> {
    match names {
        [] => None,
        [one] => Some(((*one).to_owned(), " is typing…")),
        [one, two] => Some((format!("{one} and {two}"), " are typing…")),
        _ => Some(("Several people".to_owned(), " are typing…")),
    }
}

/// The sidebar's preview for a direct conversation someone is typing in: no name where there
/// is only one other person.
#[cfg(test)]
pub fn preview(names: &[&str], group: bool) -> Option<String> {
    match (names, group) {
        ([], _) => None,
        (_, false) => Some("typing…".to_owned()),
        (names, true) => wording(names).map(|(names, rest)| format!("{names}{rest}")),
    }
}

pub struct TypingRow {
    hub: Hub,
    conversation: Option<Key>,
    /// The window is the one in use. Dots in a window nobody looks at hold still.
    active: bool,
    /// Which dot is lit.
    step: u8,
    /// When the people now typing began, without a gap.
    since: Option<Instant>,
    /// A timer is running, and which one: a new conversation leaves the old timer to end.
    running: bool,
    epoch: u32,
    #[cfg(feature = "gui-shot")]
    pub staged: Option<Vec<String>>,
}

impl TypingRow {
    pub fn new(hub: Hub) -> TypingRow {
        TypingRow {
            hub,
            conversation: None,
            active: true,
            step: 0,
            since: None,
            running: false,
            epoch: 0,
            #[cfg(feature = "gui-shot")]
            staged: None,
        }
    }

    /// Called as the composer draws: which conversation, and whether the window is in use.
    pub fn show(&mut self, conversation: Option<&Key>, active: bool, cx: &mut Context<Self>) {
        if self.conversation.as_ref() != conversation {
            self.conversation = conversation.cloned();
            self.since = None;
            self.running = false;
            self.epoch += 1;
        }
        self.active = active;
        self.run(cx);
    }

    fn typists(&self) -> Vec<Typist> {
        #[cfg(feature = "gui-shot")]
        if let Some(staged) = &self.staged {
            return staged
                .iter()
                .map(|name| Typist { id: name.clone(), name: name.clone(), until: 0, activity: Compose::Typing })
                .collect();
        }
        let Some(conversation) = &self.conversation else {
            return Vec::new();
        };
        self.hub.typists_in(conversation)
    }

    fn moving(&self) -> bool {
        #[cfg(feature = "gui-shot")]
        if self.staged.is_some() {
            return true;
        }
        self.active && self.since.is_some_and(|since| since.elapsed() < MOVING)
    }

    /// How long until this row has to draw again by itself: the next dot, or the moment the
    /// next typist times out. `None` when nobody is typing: then there is no timer at all.
    fn next(&self) -> Option<Duration> {
        let conversation = self.conversation.as_ref()?;
        let ends = self.hub.typing_ends(conversation)?;
        if self.moving() {
            return Some(STEP);
        }
        Some(Duration::from_secs((ends - now()).max(0) as u64) + Duration::from_millis(100))
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.next().is_none() {
            self.since = None;
            return;
        }
        self.since.get_or_insert_with(Instant::now);
        if std::mem::replace(&mut self.running, true) {
            return;
        }
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            loop {
                let wait = this.read_with(cx, |this, _| this.next().filter(|_| this.epoch == epoch));
                let Ok(Some(wait)) = wait else {
                    break;
                };
                cx.background_executor().timer(wait).await;
                let stepped = this.update(cx, |this, cx| {
                    if this.epoch == epoch {
                        this.step = (this.step + 1) % 3;
                        cx.notify();
                    }
                });
                if stepped.is_err() {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.epoch == epoch {
                    this.since = None;
                    this.running = false;
                    // The sidebar says "typing…" too.
                    this.hub.notify(cx);
                }
            });
        })
        .detach();
    }

    fn dots(&self) -> Div {
        let moving = self.moving();
        let lit = self.step as usize;
        div()
            .w(px(18.))
            .h(px(6.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(3.))
            .children((0..3).map(|dot| {
                let opacity = match (moving, dot == lit) {
                    (false, _) => 0.6,
                    (true, true) => 1.,
                    (true, false) => 0.35,
                };
                div().size(px(4.)).rounded(px(2.)).bg(theme::text_soft()).opacity(opacity)
            }))
    }
}

impl Render for TypingRow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let typists = self.typists();
        let row = div().h(px(HEIGHT)).flex_none().flex().items_center().gap(px(6.));
        let Some((names, rest)) = wording_for(&typists) else {
            return row;
        };
        row.type_small()
            .text_color(theme::text_soft())
            .child(self.dots())
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .child(div().truncate().font_weight(theme::MEDIUM).child(names))
                    .child(div().flex_none().child(rest)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_names_one_or_two_people_and_counts_nobody() {
        let say = |names: &[&str]| wording(names).map(|(names, rest)| format!("{names}{rest}"));
        assert_eq!(say(&[]), None);
        assert_eq!(say(&["Ines"]).as_deref(), Some("Ines is typing…"));
        assert_eq!(say(&["Ines", "Tomasz"]).as_deref(), Some("Ines and Tomasz are typing…"));
        assert_eq!(say(&["Ines", "Tomasz", "Odile"]).as_deref(), Some("Several people are typing…"));
        assert_eq!(say(&["a", "b", "c", "d", "e"]).as_deref(), Some("Several people are typing…"));
    }

    #[test]
    fn a_direct_row_says_typing_and_a_group_row_says_who() {
        assert_eq!(preview(&[], false), None);
        assert_eq!(preview(&["Ines"], false).as_deref(), Some("typing…"));
        assert_eq!(preview(&["Ines"], true).as_deref(), Some("Ines is typing…"));
    }

    #[test]
    fn activity_verbs_show_recording_uploading_sticker_and_speaking() {
        let one = |name: &str, activity: Compose| Typist { id: name.into(), name: name.into(), until: 0, activity };
        let say = |list: &[Typist]| wording_for(list).map(|(n, r)| format!("{n}{r}"));
        assert_eq!(say(&[one("Odalys", Compose::RecordingVoice)]).as_deref(), Some("Odalys is recording a voice message…"));
        assert_eq!(say(&[one("Odalys", Compose::RecordingVideo)]).as_deref(), Some("Odalys is recording a video message…"));
        assert_eq!(say(&[one("Odalys", Compose::UploadingPhoto)]).as_deref(), Some("Odalys is sending a photo…"));
        assert_eq!(say(&[one("Odalys", Compose::UploadingFile)]).as_deref(), Some("Odalys is sending a file…"));
        assert_eq!(say(&[one("Odalys", Compose::ChoosingSticker)]).as_deref(), Some("Odalys is choosing a sticker…"));
        assert_eq!(say(&[one("Odalys", Compose::Speaking)]).as_deref(), Some("Odalys is speaking…"));
        assert_eq!(preview_for(&[one("Odalys", Compose::RecordingVoice)], false).as_deref(), Some("recording a voice message…"));
        assert_eq!(preview_for(&[one("Odalys", Compose::RecordingVoice)], true).as_deref(), Some("Odalys is recording a voice message…"));
        assert_eq!(
            say(&[one("Odalys", Compose::RecordingVoice), one("Tobin", Compose::RecordingVoice)]).as_deref(),
            Some("Odalys and Tobin are recording voice messages…")
        );
        assert_eq!(say(&[one("Odalys", Compose::Cancel)]), None);
    }
}
