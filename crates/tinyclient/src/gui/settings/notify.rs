//! Settings › Notifications and the notification sounds in Settings › Sounds
//! (`docs/design/09-settings.md` §4, N1 to N13 and S2 to S4). chatd keeps every value and acts
//! on it (`chatd/src/notify.rs`); this draws them and stores what is picked.

use super::controls::{caption, card, group_label, reset_slot, row, rule};
use super::{rows, SettingsWindow};
use crate::gui::app::prefs;
use crate::gui::components::{button, glyph, ButtonKind};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, SharedString, Window};
use protocol::settings::{self as keys, Values, GLOBAL};
use protocol::{Request, Response};
use std::time::{Duration, Instant};

/// How long the test's answer stays under its button.
const TESTED_SHOWN: Duration = Duration::from_secs(8);
/// Quiet hours move by this much a press.
const QUIET_STEP: u32 = 30;

/// The words a word-valued setting takes, and how each is shown.
pub fn choices(id: &str) -> Option<(&'static str, &'static [&'static str], &'static [&'static str])> {
    Some(match id {
        "N2" => (keys::NOTIFY_ABOUT, &keys::ABOUT_CHOICES, &["Direct messages and mentions", "All messages"]),
        "N4" => (keys::PREVIEW, &keys::PREVIEW_CHOICES, &["Sender and message", "Sender only", "Nothing"]),
        "NL" => (keys::PREVIEW_LOCKED, &keys::PREVIEW_CHOICES, &["Sender and message", "Sender only", "Nothing"]),
        "N5" => (keys::CALLS, &keys::CALLS_CHOICES, &["Ring", "Silently", "Don't notify"]),
        "N6" => (keys::IN_CALL, &keys::IN_CALL_CHOICES, &["Banners only", "Banners and sounds", "Nothing"]),
        "S2" => (keys::MESSAGE_SOUND, &keys::SOUND_CHOICES, &["Pebble", "Drop", "Tap", "Chime", "Glass", "None"]),
        "S3" => (keys::MENTION_SOUND, &keys::MENTION_SOUND_CHOICES, &["Same", "Pebble", "Drop", "Tap", "Chime", "Glass", "None"]),
        "S4" => (keys::RING_SOUND, &keys::RING_CHOICES, &["Soft ring", "Classic ring", "None"]),
        _ => return None,
    })
}

/// The switches, by row: the key and its default.
fn switch_of(id: &str) -> Option<(&'static str, bool)> {
    Some(match id {
        "N1" => (keys::NOTIFY, true),
        "ND" => (keys::NOTIFY_DIRECT, true),
        "NG" => (keys::NOTIFY_GROUPS, true),
        "NM" => (keys::NOTIFY_MENTIONS, true),
        "NT" => (keys::NOTIFY_TINY, true),
        "N8" => (keys::QUIET_HOURS, false),
        "N9" => (keys::CALLS_THROUGH, true),
        "N10" => (keys::FLASH, true),
        "N11" => (keys::BADGE, true),
        _ => return None,
    })
}

/// A sound word's name, as the pickers show it.
pub fn sound_name(word: &str) -> &'static str {
    match word {
        "pebble" => "Pebble",
        "drop" => "Drop",
        "tap" => "Tap",
        "chime" => "Chime",
        "glass" => "Glass",
        "soft_ring" => "Soft ring",
        "classic_ring" => "Classic ring",
        _ => "None",
    }
}

/// "22:00", from minutes after midnight.
pub fn time_words(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60 % 24, minute % 60)
}

/// The next 08:00 local after `now`, for "Until tomorrow".
pub fn tomorrow_morning(now: i64) -> i64 {
    let local = crate::gui::time::local(now);
    let minutes = (local.hour * 60 + local.minute) as i64;
    let until = (8 * 60 - minutes).rem_euclid(24 * 60);
    let until = if until == 0 { 24 * 60 } else { until };
    now - now.rem_euclid(60) + until * 60
}

/// N7's second line: until when, or nothing when not paused.
pub fn paused_words(values: &Values, now: i64) -> Option<String> {
    match values.paused(now)? {
        None => Some("Paused until you resume".to_owned()),
        Some(until) => Some(format!("Paused until {}", crate::gui::time::clock(until))),
    }
}

/// N8's second line.
pub fn quiet_words(from: u32, to: u32) -> String {
    if from > to {
        "Every day. Crosses midnight.".to_owned()
    } else {
        "Every day.".to_owned()
    }
}

impl SettingsWindow {
    /// The Notifications section's groups.
    pub(super) fn notifications_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Div> {
        let values = prefs::values();
        let on = values.on(keys::NOTIFY);
        let mut groups = vec![self.group_of(&["N1", "N2"], None, window, cx).child(caption("Conversations you muted never notify. Do Not Disturb you set in Slack or Discord is followed here."))];
        if on {
            groups.push(self.group_of(&["ND", "NG", "NM", "NT"], Some("Notify for"), window, cx));
            groups.push(self.group_of(&["N4", "NL"], Some("Privacy"), window, cx));
        }
        groups.push(self.group_of(&["N5", "N6"], Some("Calls"), window, cx).child(caption("Ringing follows Incoming calls even when notifications are off.")));
        let mut quiet = self.group_of(&["N7", "N8"], Some("Quiet time"), window, cx);
        if values.on(keys::QUIET_HOURS) {
            quiet = quiet.child(self.quiet_hours_row(cx).mt(px(-1.)));
        }
        groups.push(quiet);
        groups.push(self.group_of(&["N9"], None, window, cx).child(caption(crate::host::FOCUS_CAPTION)));
        groups.push(self.group_of(&["N10", "N11"], Some("Attention"), window, cx));
        groups.push(self.group_of(&["N13"], Some("Test"), window, cx));
        groups
    }

    /// The sound pickers under Play sounds.
    pub(super) fn sound_groups(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Div> {
        let mut ids = vec!["S2", "S3", "S4"];
        if !rows::absent("SV") {
            ids.push("SV");
        }
        let words = if notifier::SYSTEM_PLAYS_SOUND {
            "Notification sounds use the alert volume in System Settings › Sound."
        } else {
            "tinyclient plays these itself, on the default speaker. Windows' and your desktop's notification sounds stay off for it."
        };
        vec![self.group_of(&ids, Some("Notification sounds"), window, cx).child(caption(words))]
    }

    /// A card of rows, leaving out those this system cannot honour.
    fn group_of(&mut self, ids: &[&'static str], label: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let ids: Vec<&'static str> = ids.iter().copied().filter(|id| !rows::absent(id)).collect();
        let mut drawn = card();
        for (ix, id) in ids.iter().enumerate() {
            drawn = drawn.when(ix > 0, |card| card.child(rule())).child(self.control_row(id, window, cx));
        }
        div().flex().flex_col().children(label.map(group_label)).child(drawn)
    }

    /// The rows of this module, by id; `None` for anyone else's.
    pub(super) fn notify_row(&mut self, id: &'static str, cx: &mut Context<Self>) -> Option<Div> {
        let values = prefs::values();
        if let Some((key, default)) = switch_of(id) {
            let on = values.on(key);
            let second = match (id, on) {
                ("N1", false) => Some("Counts and the badge keep working. Calls follow Incoming calls.".to_owned()),
                ("N8", true) => Some(quiet_words(values.minute(keys::QUIET_FROM), values.minute(keys::QUIET_TO))),
                _ => None,
            };
            return Some(self.switch_row(id, on, default, second, move |this, on, cx| this.set_switch(id, key, on, default, cx), cx));
        }
        if let Some((key, words, labels)) = choices(id) {
            let current_word = values.word(GLOBAL, key).to_owned();
            let default_word = Values::default().word(GLOBAL, key).to_owned();
            let current = words.iter().position(|word| *word == current_word).unwrap_or(0);
            let default = words.iter().position(|word| *word == default_word).unwrap_or(0);
            let second = self.word_second(id, &values);
            let picked = move |this: &mut SettingsWindow, ix: usize, cx: &mut Context<SettingsWindow>| {
                let word = words[ix];
                let stored = (word != default_word).then(|| word.to_owned());
                this.set(id, GLOBAL.into(), key, stored, cx);
                if matches!(id, "S2" | "S3" | "S4") {
                    this.preview_sound(id, cx);
                }
            };
            let segmented = self.segmented_row(id, labels, current, default, second, picked, cx);
            return Some(match id {
                // The play button ends the row, after the choices.
                "S2" | "S3" | "S4" => div().flex().items_center().child(segmented.flex_1()).child(div().pr(px(8.)).child(self.play_button(id, cx))),
                _ => segmented,
            });
        }
        Some(match id {
            "N7" => self.pause_row(&values, cx),
            "N13" => self.test_row(cx),
            "SV" => row(rows::find(id).map_or("", |row| row.label), None, self.slider_control(id, cx)),
            _ => return None,
        })
    }

    fn word_second(&self, id: &str, values: &Values) -> Option<String> {
        match id {
            "N4" => Some(match values.word(GLOBAL, keys::PREVIEW) {
                "sender" => "Ines Ruiz: New message",
                "nothing" => "Slack: New message",
                _ => "Ines Ruiz: Are we still on for 3?",
            }
            .to_owned()),
            "NL" => Some("While the screen is locked. The notification centre shows the same.".to_owned()),
            "N5" => Some(match values.word(GLOBAL, keys::CALLS) {
                "off" => "Calls still show in the window, and missed calls are listed.".to_owned(),
                "silent" => "Posted without a sound.".to_owned(),
                _ => format!("Plays {}, at most 4 seconds.", sound_name(values.word(GLOBAL, keys::RING_SOUND))),
            }),
            "S3" if values.word(GLOBAL, keys::MENTION_SOUND) == "same" => Some(format!("Plays {}.", sound_name(values.word(GLOBAL, keys::MESSAGE_SOUND)))),
            _ => None,
        }
    }

    /// The sound a row's play button plays now.
    fn sound_of(id: &str) -> Option<String> {
        let values = prefs::values();
        let word = match id {
            "S2" => values.word(GLOBAL, keys::MESSAGE_SOUND),
            "S3" => match values.word(GLOBAL, keys::MENTION_SOUND) {
                "same" => values.word(GLOBAL, keys::MESSAGE_SOUND),
                word => word,
            },
            _ => values.word(GLOBAL, keys::RING_SOUND),
        };
        (word != "none").then(|| word.to_owned())
    }

    /// S11: plays the row's sound once, as a notification would.
    fn preview_sound(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let Some(sound) = Self::sound_of(id) else { return };
        if !self.reachable(cx) {
            self.failed = Some(id);
            cx.notify();
            return;
        }
        self.off_thread(cx, move || crate::client::ask(&Request::PlaySound { sound }), move |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                this.failed = Some(id);
                cx.notify();
            }
        });
    }

    fn play_button(&mut self, id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let tip = match Self::sound_of(id) {
            Some(word) => format!("Play {}", sound_name(&word)),
            None => "No sound".to_owned(),
        };
        div()
            .id(SharedString::from(format!("{id}-play")))
            .size(px(28.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tab_index(0)
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(tip.clone()))
            .tooltip(crate::gui::components::tip(tip, None))
            .hover(|style| style.bg(theme::hover()))
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .child(glyph("icons/play.svg", 16., theme::text_soft()))
            .on_click(cx.listener(move |this, _, _, cx| this.preview_sound(id, cx)))
    }

    /// N7: three ways to pause, or Resume with until when.
    fn pause_row(&mut self, values: &Values, cx: &mut Context<Self>) -> Div {
        let label = rows::find("N7").map_or("", |row| row.label);
        let now = crate::view::now();
        if let Some(words) = paused_words(values, now) {
            let resume = button("N7-resume", ButtonKind::Secondary, "Resume").on_click(cx.listener(|this, _, _, cx| this.set("N7", GLOBAL.into(), keys::PAUSED_UNTIL, None, cx)));
            return row(label, Some(words.into()), resume);
        }
        let pause = |id: &'static str, words: &'static str, until: Option<String>, cx: &mut Context<Self>| {
            button(id, ButtonKind::Secondary, words).on_click(cx.listener(move |this, _, _, cx| this.set("N7", GLOBAL.into(), keys::PAUSED_UNTIL, until.clone(), cx)))
        };
        let hour = (now + 3_600).to_string();
        let morning = tomorrow_morning(now).to_string();
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(pause("N7-hour", "1 hour", Some(hour), cx))
            .child(pause("N7-tomorrow", "Until tomorrow", Some(morning), cx))
            .child(pause("N7-resume-only", "Until I resume", Some("resume".to_owned()), cx));
        row(label, None, buttons)
    }

    /// N8's times, under its switch while it is on: From [22:00] to [08:00].
    fn quiet_hours_row(&mut self, cx: &mut Context<Self>) -> Div {
        let values = prefs::values();
        let stepper = |name: &'static str, key: &'static str, cx: &mut Context<Self>| {
            let minute = values.minute(key);
            let default = Values::default().minute(key);
            let step = move |this: &mut SettingsWindow, by: i64, cx: &mut Context<SettingsWindow>| {
                let next = (i64::from(minute) + by).rem_euclid(24 * 60) as u32;
                let stored = (next != default).then(|| next.to_string());
                this.set("N8", GLOBAL.into(), key, stored, cx);
            };
            let small = |id: String, icon: &'static str, by: i64, cx: &mut Context<Self>| {
                div()
                    .id(SharedString::from(id))
                    .size(px(24.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("{name} {}", if by < 0 { "earlier" } else { "later" })))
                    .hover(|style| style.bg(theme::hover()))
                    .child(glyph(icon, 14., theme::text_soft()))
                    .on_click(cx.listener(move |this, _, _, cx| step(this, by, cx)))
            };
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .child(small(format!("{key}-earlier"), "icons/minus.svg", -i64::from(QUIET_STEP), cx))
                .child(div().w(px(44.)).flex().justify_center().type_ui().font_weight(theme::MEDIUM).child(time_words(minute)))
                .child(small(format!("{key}-later"), "icons/plus.svg", i64::from(QUIET_STEP), cx))
        };
        let from = stepper("From", keys::QUIET_FROM, cx);
        let to = stepper("To", keys::QUIET_TO, cx);
        let control = div().flex().items_center().gap(px(8.)).child(reset_slot(None)).child(from).child(div().type_ui().text_color(theme::text_soft()).child("to")).child(to);
        div().child(rule()).child(row("From", None, control))
    }

    /// N13: posts one real notification and says what came of it, for 8 s.
    fn test_row(&mut self, cx: &mut Context<Self>) -> Div {
        let label = rows::find("N13").map_or("", |row| row.label);
        let second = self.tested.as_ref().filter(|(at, _)| at.elapsed() < TESTED_SHOWN).map(|(_, words)| SharedString::from(words.clone()));
        let send = button("N13", ButtonKind::Secondary, "Send").on_click(cx.listener(|this, _, _, cx| this.send_test(cx)));
        row(label, second, send)
    }

    fn send_test(&mut self, cx: &mut Context<Self>) {
        if !self.reachable(cx) {
            self.failed = Some("N13");
            cx.notify();
            return;
        }
        self.off_thread(cx, || crate::client::ask(&Request::NotifyTest), |this, answer, cx| {
            let words = match answer {
                Ok(Response::NotifyTested { posted: true, .. }) => crate::host::TEST_SENT.to_owned(),
                Ok(Response::NotifyTested { posted: false, detail }) => detail,
                _ => "tinyclient's background service did not answer.".to_owned(),
            };
            this.tested = Some((Instant::now(), words));
            cx.notify();
            // The line goes when its time is up, with nothing else to redraw for it.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TESTED_SHOWN).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picker_offers_the_words_its_setting_takes() {
        for id in ["N2", "N4", "NL", "N5", "N6", "S2", "S3", "S4"] {
            let (key, words, labels) = choices(id).unwrap();
            assert_eq!(words.len(), labels.len(), "{id}");
            assert!(words.iter().all(|word| keys::valid(GLOBAL, key, word)), "{id}");
            assert!(rows::find(id).is_some_and(|row| row.values == labels), "{id} search values");
        }
    }

    #[test]
    fn times_and_pauses_read_as_people_say_them() {
        assert_eq!((time_words(0), time_words(22 * 60), time_words(8 * 60 + 30)), ("00:00".into(), "22:00".into(), "08:30".into()));
        assert_eq!(quiet_words(22 * 60, 8 * 60), "Every day. Crosses midnight.");
        assert_eq!(quiet_words(9 * 60, 17 * 60), "Every day.");
        let now = 1_800_000_000;
        let morning = tomorrow_morning(now);
        assert!(morning > now && morning - now <= 24 * 3_600);
        let local = crate::gui::time::local(morning);
        assert_eq!((local.hour, local.minute), (8, 0));
        let mut values = Values::default();
        assert_eq!(paused_words(&values, now), None);
        values.set(GLOBAL, keys::PAUSED_UNTIL, Some("resume".into()));
        assert_eq!(paused_words(&values, now).as_deref(), Some("Paused until you resume"));
        assert_eq!(sound_name("classic_ring"), "Classic ring");
    }
}
