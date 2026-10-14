//! The typing row above the composer: who is typing, and three dots. The dots are the only
//! thing in the window that moves by itself, so they are their own entity: a 400 ms timer changes
//! one number and repaints this row alone. The timer exists only while someone is typing, the
//! window is the one in use, and for 30 seconds at most.

use crate::gui::people::Hub;
use crate::gui::theme::{self, Type};
use crate::view::now;
use gpui::{div, prelude::*, px, Context, Div, Window};
use protocol::Key;
use std::time::{Duration, Instant};

/// The slot is always this tall, so messages never move when someone starts typing.
pub const HEIGHT: f32 = 20.;
const STEP: Duration = Duration::from_millis(400);
/// After this long of unbroken typing the dots hold still.
const MOVING: Duration = Duration::from_secs(30);

/// What the row says for the people typing, as (names, rest), so the names can be set heavier.
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

    fn names(&self) -> Vec<String> {
        #[cfg(feature = "gui-shot")]
        if let Some(staged) = &self.staged {
            return staged.clone();
        }
        let Some(conversation) = &self.conversation else {
            return Vec::new();
        };
        self.hub.typists_in(conversation).into_iter().map(|typist| typist.name).collect()
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
        let names = self.names();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let row = div().h(px(HEIGHT)).flex_none().flex().items_center().gap(px(6.));
        let Some((names, rest)) = wording(&names) else {
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
}
