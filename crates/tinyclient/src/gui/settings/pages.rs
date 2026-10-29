//! What each section draws (`docs/design/09-settings.md` §4), the search results (§7), the
//! modals and the device menus. Every row is built by `control_row` from its id, so a search
//! result is the same live row its section shows.

use super::accounts::network_name;
use super::controls::{self, caption, card, group_label, reset_button, reset_slot, row, rule, slider_track, slider_value, switch};
use super::rows::{self, Row};
use super::{modal_panel, Confirm, Picker, Section, SettingsWindow};
use crate::gui::anim::{self, Autoplay};
use crate::gui::app::prefs::{self, Choices};
use crate::gui::components::{button, glyph, keycap, menu, menu_item_check, menu_separator, network_icon, placed, segment, segmented, ButtonKind, Layer, Place};
use crate::gui::layout::NETWORK_ORDER;
use crate::gui::theme::{self, Type};
use gpui::{canvas, div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, MouseButton, SharedString, Window};
use crate::gui::hotkeys::{self, Action, Reach};
use protocol::settings::{
    self as keys, account_scope, Values, AUTO_GAIN, CACHE_MB, CACHE_MB_CHOICES, CUE_CONNECTION, CUE_JOINS, CUE_MINE, CUE_VOLUME,
    ECHO_CANCELLATION, GLOBAL, INPUT, JOIN_MUTED, KEEP_DAYS, KEEP_DAYS_CHOICES, KEEP_RUNNING, NOISE_SUPPRESSION, OUTPUT, PTT_RELEASE_CHOICES,
    PTT_RELEASE_MS, PUSH_TO_TALK, RECEIPTS, SENSITIVITY, SOUNDS, TYPING, VOICE_VOLUME,
};
use protocol::{AccountStatus, Link, Network, Request, Response};
use std::rc::Rc;

/// How a slider reads, stores and labels its value.
pub struct Slider {
    pub range: (u32, u32),
    pub default: u32,
    pub read: fn() -> u32,
    pub stops: Option<u32>,
    pub width: f32,
    pub words: fn(u32) -> String,
}

pub fn slider(id: &str) -> Slider {
    let percent = |value: u32| format!("{value} %");
    match id {
        // Text size: five stops, −1 to +3, stored as 0 to 4.
        "A2" => Slider { range: (0, 4), default: 1, read: || (theme::text_step() + 1) as u32, stops: Some(5), width: controls::TRACK, words: |v| format!("{} pt", 13 + v) },
        // Narrower than the others: its row also carries the test sound's button.
        "S10" => Slider { range: (0, 100), default: 50, read: || prefs::values().number(CUE_VOLUME), stops: None, width: 160., words: percent },
        "V5" => Slider { range: (0, 100), default: 100, read: || prefs::values().number(VOICE_VOLUME), stops: None, width: controls::TRACK, words: percent },
        // Release delay: one stop for each of the six choices.
        "V14" => Slider { range: (0, 5), default: 2, read: release_stop, stops: Some(6), width: controls::TRACK, words: release_words },
        _ => Slider { range: (0, 100), default: 25, read: || prefs::values().number(SENSITIVITY), stops: None, width: controls::METER, words: |v| v.to_string() },
    }
}

pub fn store_slider(window: &mut SettingsWindow, id: &'static str, value: u32, cx: &mut Context<SettingsWindow>) {
    let key = match id {
        "A2" => return window.set_text_step(value as i32 - 1, cx),
        "S10" => CUE_VOLUME,
        "V5" => VOICE_VOLUME,
        "V14" => {
            let ms = PTT_RELEASE_CHOICES[(value as usize).min(PTT_RELEASE_CHOICES.len() - 1)];
            let stored = (ms != Values::default_number(PTT_RELEASE_MS)).then(|| ms.to_string());
            return window.set(id, GLOBAL.into(), PTT_RELEASE_MS, stored, cx);
        }
        _ => SENSITIVITY,
    };
    // Only what differs from the default is stored.
    let stored = (value != Values::default_number(key)).then(|| value.to_string());
    window.set(id, GLOBAL.into(), key, stored, cx);
}

/// V14's stop for the release delay stored.
fn release_stop() -> u32 {
    let ms = prefs::values().number(PTT_RELEASE_MS);
    PTT_RELEASE_CHOICES.iter().position(|choice| *choice == ms).unwrap_or(2) as u32
}

fn release_words(stop: u32) -> String {
    match PTT_RELEASE_CHOICES.get(stop as usize) {
        Some(1000) => "1 s".to_owned(),
        Some(ms) => format!("{ms} ms"),
        None => String::new(),
    }
}

/// The key action a key row records.
pub fn key_action(id: &str) -> Option<Action> {
    Some(match id {
        "V13" => Action::Talk,
        "V15" => Action::Priority,
        "V16" => Action::Mute,
        "V17" => Action::Deafen,
        _ => return None,
    })
}

/// What the Keys group says the keys can reach on this system.
fn keys_caption() -> &'static str {
    match hotkeys::reach() {
        Reach::Everywhere => "These keys work in any app, even with tinyclient in the background.",
        Reach::Permission => "To work in other apps, these keys need Input Monitoring in System Settings › Privacy & Security.",
        Reach::Window => "This system shows keys only to the app in front: they work while tinyclient's window is, and push to talk needs an X11 session.",
    }
}

/// The length of the test sound (`Cue::Test`), for its button's playing state.
const TEST_CUE: std::time::Duration = std::time::Duration::from_millis(180);

/// What the line under a row says when what it asked did not happen.
fn failure_words(row: &str, reachable: bool) -> &'static str {
    match (row, reachable) {
        ("S12", false) => "Couldn't play: tinyclient's background service is not answering.",
        ("S12", true) => "Couldn't play the test sound. Try again.",
        ("V3", _) => "Couldn't start the test. Try again.",
        ("D3", _) => "Couldn't clear. Try again.",
        ("B3", _) => "Couldn't reset every setting. Try again.",
        _ => "Couldn't save. Try again.",
    }
}

fn label_of(id: &str) -> &'static str {
    rows::find(id).map_or("", |row| row.label)
}

/// The accounts that have rows of their own, in the rail's order.
fn accounts(window: &SettingsWindow, cx: &gpui::App) -> Vec<AccountStatus> {
    let Some(shell) = window.shell.upgrade() else {
        return Vec::new();
    };
    let held = shell.read(cx).snapshot.as_ref().map(|s| s.accounts.clone()).unwrap_or_default();
    let mut accounts: Vec<AccountStatus> = held.into_iter().filter(|a| a.link != Link::SignedOut).collect();
    accounts.sort_by_key(|a| NETWORK_ORDER.iter().position(|n| *n == a.key.network));
    accounts
}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024. * 1024.);
    if mb >= 1024. {
        format!("{:.1} GB", mb / 1024.)
    } else if mb >= 10. {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// D1's numbers, messages, files and the total, all at the total's precision, with the total
/// the sum of the two parts as shown: the legend adds up to the heading. A part too small to
/// show at that precision says so rather than reading as nothing.
pub fn usage_words(messages: u64, files: u64) -> (String, String, String) {
    let mb = |bytes: u64| bytes as f64 / (1024. * 1024.);
    let total = mb(messages + files);
    let (unit, per, decimals) = if total >= 1024. { ("GB", 1024., 1) } else if total >= 10. { ("MB", 1., 0) } else { ("MB", 1., 1) };
    let step = 10f64.powi(-decimals);
    let round = |bytes: u64| (mb(bytes) / per / step).round() * step;
    let show = |value: f64| format!("{value:.prec$} {unit}", prec = decimals as usize);
    let part = |bytes: u64| if bytes > 0 && round(bytes) == 0. { format!("under {}", show(step)) } else { show(round(bytes)) };
    (part(messages), part(files), show(round(messages) + round(files)))
}

/// The P4 modal's sentence.
pub fn retention_words(prunable: Option<(u64, u64)>) -> String {
    let rest = crate::host::removed_from_here();
    match prunable {
        Some((0, _)) => crate::host::retention_none_yet(),
        Some((messages, bytes)) => {
            let count = messages.to_string();
            // Thousands get a comma, as everywhere else in the app.
            let mut grouped = String::new();
            for (ix, digit) in count.chars().enumerate() {
                if ix > 0 && (count.len() - ix).is_multiple_of(3) {
                    grouped.push(',');
                }
                grouped.push(digit);
            }
            let noun = if messages == 1 { "message" } else { "messages" };
            let verb = if messages == 1 { "is removed" } else { "are removed" };
            let rest = rest.replacen("are removed", verb, 1);
            let rest = if messages == 1 { rest.replacen("They stay", "It stays", 1) } else { rest };
            // A size under a tenth of a megabyte says nothing worth reading.
            if bytes < 100 * 1024 {
                format!("{grouped} {noun} {rest}")
            } else {
                format!("{grouped} {noun}, about {}, {rest}", megabytes(bytes))
            }
        }
        None => format!("Older messages {rest}"),
    }
}

impl SettingsWindow {
    pub(super) fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let column = div().w(px(controls::COLUMN)).flex().flex_col();
        let groups: Vec<Div> = match self.section {
            Section::Accounts => return column.child(self.accounts(window, cx)),
            Section::General => vec![
                self.group(&["G2"], None, window, cx).child(caption(self.keep_running_caption())),
                self.group(&["G4"], None, window, cx).child(caption(self.updates_caption())),
            ],
            Section::Sounds => vec![self.group(&["S1"], None, window, cx).child(caption("Which call sounds play, and how loud, is in Voice."))],
            Section::Appearance => vec![
                self.group(&["A2", "A3", "A4"], None, window, cx)
                    .child(caption(crate::host::APPEARANCE_CAPTION)),
            ],
            Section::Messages => vec![self.group(&["M1", "M2", "M3", "M4", "M5"], None, window, cx)],
            Section::Privacy => {
                let mut groups = self.per_account(window, cx);
                groups.push(self.group(&["P3"], None, window, cx));
                groups.push(
                    self.group(&["P4"], None, window, cx)
                        .child(caption("Older messages are deleted here, never on the networks. Each conversation also keeps at most its newest 2,000.")),
                );
                groups
            }
            Section::Audio => vec![
                self.group(&["V1"], Some("Microphone"), window, cx).child(caption("How your microphone opens, its level and its processing are in Voice.")),
                self.group(&["V2", "V5"], Some("Speaker"), window, cx)
                    .child(caption("Picking a device in a call's menu changes that call only. These are the defaults.")),
                self.group(&["V4"], Some("Joining"), window, cx),
            ],
            Section::Voice => {
                let values = prefs::values();
                let push = values.on(PUSH_TO_TALK);
                let mut input = vec!["V8", "V3", "V18"];
                let mode = if push {
                    input.push("V14");
                    if values.hotkey(keys::PTT_KEY).is_none() {
                        "Pick a push-to-talk key below: until then your microphone never opens."
                    } else {
                        "Your microphone opens while the key is held. A room that requires push to talk uses it, whatever you pick here."
                    }
                } else {
                    "A room that requires push to talk uses it, whatever you pick here."
                };
                input.push("V19");
                vec![
                    self.group(&input, None, window, cx).child(caption(mode)),
                    self.group(&["V13", "V15", "V16", "V17"], Some("Keys"), window, cx).child(caption(keys_caption())),
                    self.group(&["V10", "V11", "V12"], Some("Processing"), window, cx)
                        .child(caption("Echo cancellation takes out what your speakers play, so others don't hear themselves when you talk without headphones.")),
                    self.group(&["S7", "S8", "S9", "S10"], Some("Call sounds"), window, cx)
                        .child(caption("Call sounds play on your call speaker, quieter than voices. Voice volume is in Audio & video.")),
                ]
            }
            Section::Storage => vec![self.group(&["D1", "D2", "D3"], None, window, cx).child(caption("Sign-ins, drafts and settings are not part of this."))],
            Section::Shortcuts => return column.child(self.shortcuts("")),
            Section::About => return column.child(self.about(window, cx)),
        };
        let banner = self.banner(cx);
        column.children(banner).children(groups.into_iter().enumerate().map(|(ix, group)| group.when(ix > 0, |group| group.pt(px(20.)))))
    }

    /// A banner over sections whose rows chatd keeps, when chatd does not answer.
    fn banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let chatd = matches!(self.section, Section::General | Section::Sounds | Section::Privacy | Section::Audio | Section::Voice | Section::Storage);
        let denied = self.section == Section::Audio && self.devices.as_ref().is_some_and(|d| d.microphone == "denied");
        let unwatched = self.section == Section::Voice && hotkeys::reach() == Reach::Permission;
        let (icon, words, action): (&'static str, &'static str, Option<(&'static str, &'static str)>) = if chatd && !self.reachable(cx) {
            ("icons/wifi-off.svg", "Can't reach tinyclient's background service. These settings return when it does.", None)
        } else if denied {
            (
                "icons/mic-off.svg",
                "tinyclient can't use the microphone. Allow it in System Settings › Privacy & Security › Microphone.",
                Some(("Open System Settings", "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")),
            )
        } else if unwatched {
            (
                "icons/key.svg",
                "Your voice keys work only in tinyclient until you allow Input Monitoring for it.",
                Some(("Open System Settings", "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")),
            )
        } else {
            return None;
        };
        Some(
            div().pb(px(16.)).child(
                div()
                    .min_h(px(40.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(theme::raised())
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph(icon, 16., theme::warning()))
                    .child(div().flex_1().type_ui().text_color(theme::text()).child(words))
                    .children(action.map(|(label, url)| {
                        button("banner-action", ButtonKind::Secondary, label).h(px(24.)).on_click(cx.listener(move |_, _, _, cx| cx.open_url(url)))
                    })),
            ),
        )
    }

    fn updates_caption(&self) -> &'static str {
        if prefs::choices().updates {
            "A newer version is downloaded while tinyclient is open and installed when you quit."
        } else {
            "tinyclient tells you when a newer version is published. Nothing is downloaded until you ask."
        }
    }

    fn set_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        self.choose(|choices| choices.updates = on, cx);
        if on {
            self.off_thread(
                cx,
                || crate::update::pursue(crate::update::Policy::Now { automatic: true }),
                |this, _, cx| {
                    let _ = this.shell.update(cx, |shell, cx| {
                        if crate::update::announced() {
                            shell.layout_state.update_alert = true;
                        }
                        cx.notify();
                    });
                    cx.notify();
                },
            );
        } else {
            crate::update::discard_download();
        }
    }

    fn keep_running_caption(&self) -> &'static str {
        if prefs::values().on(KEEP_RUNNING) {
            "Closing the window leaves tinyclient running. Quit with ⌘Q to stop it."
        } else {
            "Closing the window quits tinyclient. Nothing arrives until you open it again."
        }
    }

    /// A card of rows, with its label above.
    fn group(&mut self, ids: &[&'static str], label: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut card = card();
        for (ix, id) in ids.iter().enumerate() {
            card = card.when(ix > 0, |card| card.child(rule())).child(self.control_row(id, window, cx));
        }
        div().flex().flex_col().children(label.map(group_label)).child(card)
    }

    /// One row by its id, live: the same in its section and in search results.
    pub(super) fn control_row(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let values = prefs::values();
        let choices = prefs::choices();
        let label = label_of(id);
        let built = match id {
            "G2" => {
                let on = values.on(KEEP_RUNNING);
                self.switch_row(id, on, true, None, move |this, on, cx| this.set_switch(id, KEEP_RUNNING, on, true, cx), cx)
            }
            "G4" => {
                let on = choices.updates;
                self.switch_row(id, on, true, None, move |this, on, cx| this.set_updates(on, cx), cx)
            }
            "S1" => {
                let on = values.on(SOUNDS);
                let second = (!on).then(|| "Calls make no sounds. Your choices below are kept.".to_owned());
                self.switch_row(id, on, true, second, move |this, on, cx| this.set_switch(id, SOUNDS, on, true, cx), cx)
            }
            "S7" | "S8" | "S9" => {
                let key = match id {
                    "S7" => CUE_JOINS,
                    "S8" => CUE_MINE,
                    _ => CUE_CONNECTION,
                };
                let on = values.on(key);
                self.switch_row(id, on, true, None, move |this, on, cx| this.set_switch(id, key, on, true, cx), cx)
            }
            "S10" => {
                let output = self.devices.as_ref().map(|d| d.default_output.clone()).unwrap_or_default();
                let device = values.device(OUTPUT).unwrap_or(output);
                let playing = self.playing.is_some();
                let tip = match (playing, device.is_empty()) {
                    (true, _) => "Playing test sound".to_owned(),
                    (false, true) => "Play test sound".to_owned(),
                    (false, false) => format!("Play test sound on {device}"),
                };
                let play = div()
                    .id("S12")
                    .ml(px(4.))
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
                    // The square while the cue plays, so a click that is heard as nothing can
                    // be told from one that did nothing.
                    .child(glyph(if playing { "icons/square.svg" } else { "icons/play.svg" }, 16., if playing { theme::accent() } else { theme::text_soft() }))
                    .on_click(cx.listener(|this, _, _, cx| this.play_test_sound(cx)));
                row(label, None, div().flex().items_center().child(self.slider_control(id, cx)).child(play))
            }
            "A2" => row(label, None, self.slider_control(id, cx)),
            "A3" => {
                let second = choices.compact.then(|| "List rows are 40 instead of 48.".to_owned());
                self.segmented_row(id, &["Comfortable", "Compact"], usize::from(choices.compact), 0, second, |this, ix, cx| this.choose(|c| c.compact = ix == 1, cx), cx)
            }
            "A4" => {
                let second = (!choices.previews).then(|| "Rows show names and times only.".to_owned());
                self.switch_row(id, choices.previews, true, second, |this, on, cx| this.choose(|c| c.previews = on, cx), cx)
            }
            "M1" => {
                let second = if choices.command_return { "Return adds a line." } else { "⇧Return adds a line." };
                self.segmented_row(id, &["Return", "⌘Return"], usize::from(choices.command_return), 0, Some(second.to_owned()), |this, ix, cx| this.choose(|c| c.command_return = ix == 1, cx), cx)
            }
            "M2" => self.segmented_row(id, &["First unread", "Newest message"], usize::from(choices.newest), 0, None, |this, ix, cx| this.choose(|c| c.newest = ix == 1, cx), cx),
            "M3" => self.switch_row(id, choices.typing, true, None, |this, on, cx| this.choose(|c| c.typing = on, cx), cx),
            "M4" => {
                let second = "When you send, :) :( :D ;) <3 become 🙂 🙁 😀 😉 ❤️. Never inside code.".to_owned();
                self.switch_row(id, choices.emoticons, false, Some(second), |this, on, cx| this.choose(|c| c.emoticons = on, cx), cx)
            }
            "M5" => {
                let at = anim::autoplay() as usize;
                let second = match (anim::autoplay(), crate::gui::app::reduce_motion()) {
                    (Autoplay::Never, _) => Some("Click one to play it.".to_owned()),
                    (Autoplay::Always, true) => Some("Reduce Motion is on, so they play under the pointer.".to_owned()),
                    _ => None,
                };
                self.segmented_row(id, &["Automatically", "On hover", "Never"], at, 0, second, |this, ix, cx| {
                    this.set_autoplay([Autoplay::Always, Autoplay::Hover, Autoplay::Never][ix], cx)
                }, cx)
            }
            "P3" => {
                let second = (!choices.images).then(|| "Nothing is fetched until you click it.".to_owned());
                self.switch_row(id, choices.images, true, second, |this, on, cx| this.choose(|c| c.images = on, cx), cx)
            }
            "P4" => {
                let days = values.number(KEEP_DAYS);
                let at = KEEP_DAYS_CHOICES.iter().position(|d| *d == days).unwrap_or(2);
                self.segmented_row(id, &["7 days", "30 days", "90 days"], at, 2, None, move |this, ix, cx| this.pick_retention(KEEP_DAYS_CHOICES[ix], cx), cx)
            }
            "P1" | "P2" => return self.per_account_rows(id, cx),
            "V1" | "V2" => self.device_row(id, cx),
            "V3" => self.meter_row(cx),
            "V8" => {
                let push = values.on(PUSH_TO_TALK);
                self.segmented_row(id, &["Voice activation", "Push to talk"], usize::from(push), 0, None, move |this, ix, cx| this.set_switch(id, PUSH_TO_TALK, ix == 1, false, cx), cx)
            }
            "V14" => row(label, None, self.slider_control(id, cx)),
            "V19" => {
                let on = values.on(keys::MUTED_TALK);
                let second = Some("Your microphone listens while you're muted, only to say so. Nothing is sent.".to_owned());
                self.switch_row(id, on, true, second, move |this, on, cx| this.set_switch(id, keys::MUTED_TALK, on, true, cx), cx)
            }
            "V18" => {
                let second = Some("Plays your microphone back as others would hear it. Not during a call.".to_owned());
                self.switch_row(id, self.loopback, false, second, |this, on, cx| this.set_loopback(on, cx), cx)
            }
            "V10" | "V11" | "V12" => {
                let key = match id {
                    "V10" => ECHO_CANCELLATION,
                    "V11" => NOISE_SUPPRESSION,
                    _ => AUTO_GAIN,
                };
                let on = values.on(key);
                let second = match (id, on) {
                    ("V10", false) => Some("Wear headphones, or others hear themselves when you talk."),
                    ("V11", false) => Some("Fans, typing and traffic go through as they are."),
                    ("V12", false) => Some("Your voice is sent at the level your microphone gives."),
                    _ => None,
                };
                self.switch_row(id, on, true, second.map(str::to_owned), move |this, on, cx| this.set_switch(id, key, on, true, cx), cx)
            }
            "V13" | "V15" | "V16" | "V17" => return self.key_row(id, cx),
            "V6" => row(label, None, self.slider_control(id, cx)),
            "V5" => row(label, None, self.slider_control(id, cx)),
            "V4" => {
                let on = values.on(JOIN_MUTED);
                self.switch_row(id, on, false, None, move |this, on, cx| this.set_switch(id, JOIN_MUTED, on, false, cx), cx)
            }
            "D1" => self.usage(cx),
            "D2" => {
                let cap = values.number(CACHE_MB);
                let at = CACHE_MB_CHOICES.iter().position(|mb| *mb == cap).unwrap_or(1);
                let second = Some("The oldest unused go first.".to_owned());
                self.segmented_row(id, &["250 MB", "500 MB", "1 GB", "2 GB"], at, 1, second, move |this, ix, cx| {
                    let mb = CACHE_MB_CHOICES[ix];
                    let stored = (mb != Values::default_number(CACHE_MB)).then(|| mb.to_string());
                    this.set("D2", GLOBAL.into(), CACHE_MB, stored, cx);
                }, cx)
            }
            "D3" => row(
                label,
                None,
                button("D3", ButtonKind::Danger, "Clear…").on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = Some(Confirm::Clear);
                    cx.notify();
                })),
            ),
            "B1" => row(
                label,
                Some(crate::host::DIAGNOSTICS.into()),
                button("B1", ButtonKind::Secondary, "Copy").on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx))),
            ),
            "B2" => row(
                label,
                None,
                button("B2", ButtonKind::Secondary, crate::host::REVEAL).on_click(cx.listener(|_, _, _, cx| {
                    let log = protocol::support_dir().join(protocol::launch::CHATD_LOG);
                    if log.exists() {
                        cx.reveal_path(&log);
                    } else {
                        cx.open_with_system(&protocol::support_dir());
                    }
                })),
            ),
            "B3" => row(
                label,
                None,
                button("B3", ButtonKind::Danger, "Reset…").on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = Some(Confirm::Reset);
                    cx.notify();
                })),
            ),
            _ => div(),
        };
        let _ = window;
        let lit = self.highlight == Some(id);
        let failed = self.failed.filter(|row| *row == id || (id == "S10" && *row == "S12")).map(|row| failure_words(row, self.reachable(cx))).map(|words| div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::danger()).child(words));
        // Disabled while chatd cannot be reached: `set` takes no change then, and the banner
        // over the section says why.
        let waiting = self.waits_on_chatd(id, cx);
        div().flex().flex_col().rounded(px(theme::R_CARD)).when(lit, |row| row.bg(theme::selected())).when(waiting, |row| row.opacity(0.4)).child(built).children(failed)
    }

    /// S12: the join cue at the volume set, on the call speaker. Says so if it cannot be asked.
    fn play_test_sound(&mut self, cx: &mut Context<Self>) {
        if !self.reachable(cx) {
            self.failed = Some("S12");
            cx.notify();
            return;
        }
        self.failed = None;
        let started = std::time::Instant::now();
        self.playing = Some(started);
        cx.notify();
        self.off_thread(cx, || crate::client::ask(&Request::TestSound), move |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                this.playing = None;
                this.failed = Some("S12");
                cx.notify();
                return;
            }
            // The cue is 180 ms from when mediad was asked.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TEST_CUE).await;
                let _ = this.update(cx, |this, cx| {
                    if this.playing == Some(started) {
                        this.playing = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        });
    }

    fn set_switch(&mut self, row: &'static str, key: &'static str, on: bool, default: bool, cx: &mut Context<Self>) {
        let stored = (on != default).then(|| if on { "on" } else { "off" }.to_owned());
        self.set(row, GLOBAL.into(), key, stored, cx);
    }

    /// A row whose whole width toggles its switch, with the reset slot before it.
    fn switch_row(
        &mut self,
        id: &'static str,
        on: bool,
        default: bool,
        second: Option<String>,
        toggle: impl Fn(&mut SettingsWindow, bool, &mut Context<SettingsWindow>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let label = label_of(id);
        let toggle = Rc::new(toggle);
        let (by_row, by_switch, by_reset) = (toggle.clone(), toggle.clone(), toggle);
        let reset = (on != default).then(|| {
            reset_button(SharedString::from(format!("{id}-reset")), format!("Reset {label} to {}", if default { "on" } else { "off" }).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    by_reset(this, default, cx);
                }))
        });
        let control = div().flex().items_center().child(reset_slot(reset)).child(switch(SharedString::from(format!("{id}-switch")), label, on).on_click(
            cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                by_switch(this, !on, cx);
            }),
        ));
        div().child(
            div()
                .id(SharedString::from(format!("{id}-row")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| by_row(this, !on, cx)))
                .child(row(label, second.map(SharedString::from), control)),
        )
    }

    /// Two to four choices, ← → moving between them when it has the keys.
    #[allow(clippy::too_many_arguments)]
    fn segmented_row(
        &mut self,
        id: &'static str,
        choices: &[&'static str],
        current: usize,
        default: usize,
        second: Option<String>,
        pick: impl Fn(&mut SettingsWindow, usize, &mut Context<SettingsWindow>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let label = label_of(id);
        let pick = Rc::new(pick);
        let count = choices.len();
        let mut control = segmented()
            .id(SharedString::from(format!("{id}-choices")))
            .tab_index(0)
            .role(gpui::Role::RadioGroup)
            .aria_label(label)
            .focus_visible(|style| style.shadow(theme::focus_ring()));
        let keys = pick.clone();
        control = control.on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
            let next = match event.keystroke.key.as_str() {
                "left" if current > 0 => current - 1,
                "right" if current + 1 < count => current + 1,
                _ => return,
            };
            cx.stop_propagation();
            keys(this, next, cx);
        }));
        for (ix, choice) in choices.iter().enumerate() {
            let pick = pick.clone();
            control = control.child(
                segment(SharedString::from(format!("{id}-{ix}")), *choice, ix == current)
                    .role(gpui::Role::RadioButton)
                    .aria_label(crate::gui::shortcuts::label(choice))
                    .aria_selected(ix == current)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if ix != current {
                            pick(this, ix, cx);
                        }
                    })),
            );
        }
        let reset_to = pick.clone();
        let reset = (current != default).then(|| {
            reset_button(SharedString::from(format!("{id}-reset")), format!("Reset {label} to {}", choices[default]).into())
                .on_click(cx.listener(move |this, _, _, cx| reset_to(this, default, cx)))
        });
        row(label, second.map(SharedString::from), div().flex().items_center().child(reset_slot(reset)).child(control))
    }

    /// The track, its value and its reset, for slider `id`.
    fn slider_control(&mut self, id: &'static str, cx: &mut Context<Self>) -> Div {
        let spec = slider(id);
        let stored = (spec.read)();
        let value = self.pending.get(id).copied().unwrap_or(stored);
        let anchors = self.anchors.clone();
        let label = label_of(id);
        let reset = (stored != spec.default).then(|| {
            reset_button(SharedString::from(format!("{id}-reset")), format!("Reset {label} to {}", (spec.words)(spec.default)).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pending.remove(id);
                    let default = slider(id).default;
                    store_slider(this, id, default, cx);
                }))
        });
        let track = div()
            .id(SharedString::from(format!("{id}-slider")))
            .relative()
            .cursor_pointer()
            .tab_index(0)
            .role(gpui::Role::Slider)
            .aria_label(label)
            .aria_numeric_value(f64::from(value))
            .aria_min_numeric_value(f64::from(spec.range.0))
            .aria_max_numeric_value(f64::from(spec.range.1))
            .aria_numeric_value_step(1.)
            .aria_value((spec.words)(value))
            .rounded(px(theme::R_CONTROL))
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.drag = Some(id);
                    this.slide_to(id, event.position.x, cx);
                }),
            )
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                if this.slider_key(id, event.keystroke.key.as_str(), event.keystroke.modifiers.alt, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(slider_track(value, spec.range, spec.stops, Some(spec.default), spec.width))
            .child(canvas(move |bounds, _, _| { anchors.borrow_mut().insert(id, bounds); }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let ends = |size: f32| div().w(px(16.)).flex().justify_center().type_ui().text_size(px(size)).text_color(theme::text_soft()).child("A");
        // The thumb overhangs each end of the track by 8: 16 keeps 8 clear of the glyphs.
        let track = if id == "A2" { div().flex().items_center().gap(px(controls::VALUE_GAP)).child(ends(11.)).child(track).child(ends(16.)) } else { div().child(track) };
        div().flex().items_center().child(reset_slot(reset)).child(track).child(div().w(px(controls::VALUE_GAP))).child(slider_value((spec.words)(value)))
    }

    /// P1 and P2: a card per setting, one row per account, fixed rows where the network decides.
    fn per_account(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Div> {
        let accounts = accounts(self, cx);
        if accounts.is_empty() {
            return vec![div().flex().flex_col().child(group_label("Let others see")).child(caption("Sign in to an account to choose what it shares.").pt(px(0.)))];
        }
        let _ = window;
        let typing = div().flex().flex_col().child(group_label(label_of("P1"))).child(self.per_account_rows("P1", cx));
        let receipts = div()
            .pt(px(20.))
            .flex()
            .flex_col()
            .child(group_label(label_of("P2")))
            .child(self.per_account_rows("P2", cx))
            .child(caption("Discord and Slack have no read receipts."));
        vec![typing, receipts]
    }

    fn per_account_rows(&mut self, id: &'static str, cx: &mut Context<Self>) -> Div {
        let values = prefs::values();
        let accounts = accounts(self, cx);
        let several = |network: Network| accounts.iter().filter(|a| a.key.network == network).count() > 1;
        let mut card = card();
        let mut drawn = 0;
        for (ix, account) in accounts.iter().enumerate() {
            let network = account.key.network;
            // Fixed rows say what the network decides; Discord and Slack have no receipts.
            let (fixed, reason): (Option<&str>, Option<&str>) = match (id, network) {
                ("P1", Network::Telegram) | ("P2", Network::Telegram) => (Some("Always on"), Some("Telegram does not let an app turn this off.")),
                ("P1", Network::Signal) | ("P2", Network::Signal) => (Some("Set on your phone"), Some("Signal takes this from your phone's settings.")),
                ("P1", Network::Messages) => (Some("Sent to the phone"), Some("Messages shows typing on your other devices.")),
                ("P2", Network::Discord | Network::Slack) | ("P1" | "P2", Network::Tiny) => continue,
                _ => (None, None),
            };
            let name = if account.name.is_empty() { network_name(network).to_owned() } else { account.name.clone() };
            let second = if let Some(reason) = reason {
                Some(reason.to_owned())
            } else if id == "P2" {
                Some("Off also leaves chats unread on your other devices.".to_owned())
            } else if several(network) {
                Some(network_name(network).to_owned())
            } else {
                None
            };
            let lead = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(account.link == Link::Expired, |lead| lead.opacity(0.4))
                .child(network_icon(network, 16.))
                .child(div().type_ui().text_color(theme::text()).child(name.clone()));
            let control: AnyElement = match fixed {
                Some(words) => div().type_ui().text_color(theme::text_soft()).child(words).into_any_element(),
                None => {
                    let scope = account_scope(&account.key);
                    let (key, on, default) = if id == "P1" {
                        (TYPING, values.typing(&account.key), keys::typing_default(network))
                    } else {
                        (RECEIPTS, values.receipts(&account.key), true)
                    };
                    let reset = (on != default).then(|| {
                        let scope = scope.clone();
                        reset_button(SharedString::from(format!("{id}-{ix}-reset")), format!("Reset {name} to {}", if default { "on" } else { "off" }).into())
                            .on_click(cx.listener(move |this, _, _, cx| this.set(id, scope.clone(), key, None, cx)))
                    });
                    let toggle = move |this: &mut SettingsWindow, on: bool, scope: String, cx: &mut Context<SettingsWindow>| {
                        let stored = (on != default).then(|| if on { "on" } else { "off" }.to_owned());
                        this.set(id, scope, key, stored, cx);
                    };
                    div()
                        .flex()
                        .items_center()
                        .child(reset_slot(reset))
                        .child(switch(SharedString::from(format!("{id}-{ix}")), &name, on).on_click(cx.listener(move |this, _, _, cx| toggle(this, !on, scope.clone(), cx))))
                        .into_any_element()
                }
            };
            let step = theme::text_step().max(0) as f32 * 2.;
            let line = div()
                .min_h(px(if second.is_some() { 52. } else { 40. } + step))
                .px(px(12.))
                .py(px(if second.is_some() { 8. } else { 6. }))
                .flex()
                .items_center()
                .gap(px(16.))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(lead).children(second.map(|line| div().pl(px(24.)).type_small().text_color(theme::text_soft()).child(line))))
                .child(control);
            card = card.when(drawn > 0, |card| card.child(rule())).child(line);
            drawn += 1;
        }
        let failed = (self.failed == Some(id)).then(|| div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::danger()).child(failure_words(id, true)));
        card.when(self.waits_on_chatd(id, cx), |card| card.opacity(0.4)).children(failed)
    }

    /// The menu button of a device picker (V1, V2). As wide as what it says, 120 to 240; the
    /// whole name is in its tooltip and in the menu.
    fn device_row(&mut self, id: &'static str, cx: &mut Context<Self>) -> Div {
        let (key, picker, noun) = if id == "V1" { (INPUT, Picker::Input, "microphone") } else { (OUTPUT, Picker::Output, "speaker") };
        let stored = prefs::values().device(key);
        let devices = self.devices.clone().unwrap_or_default();
        let (listed, default) = if id == "V1" { (&devices.inputs, &devices.default_input) } else { (&devices.outputs, &devices.default_output) };
        let gone = stored.as_ref().is_some_and(|name| self.devices.is_some() && !listed.contains(name));
        let (shown, whole) = match &stored {
            Some(name) if gone => (format!("{name} (not connected)"), format!("{name} (not connected)")),
            Some(name) => (name.clone(), name.clone()),
            None if listed.is_empty() && self.devices.is_some() => (format!("No {noun} found"), format!("No {noun} found")),
            None if default.is_empty() => ("System default".to_owned(), "System default".to_owned()),
            // The device the default is now is the menu's to say: here it only crowds the name.
            None => ("System default".to_owned(), format!("System default: {default}")),
        };
        let second = gone.then(|| "Calls use the system default until it is back.".into());
        let anchors = self.anchors.clone();
        let anchor: &'static str = if id == "V1" { "V1-menu" } else { "V2-menu" };
        let label = label_of(id);
        let picker_button = div()
            .id(SharedString::from(format!("{id}-picker")))
            .track_focus(&self.picker_focus[picker as usize])
            .min_w(px(120.))
            .max_w(px(240.))
            .h(px(theme::CONTROL))
            .pl(px(10.))
            .pr(px(8.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::hover())
            .border_1()
            .border_color(theme::border())
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .role(gpui::Role::ComboBox)
            .aria_label(label)
            .aria_value(SharedString::from(whole.clone()))
            .tooltip(crate::gui::components::tip(whole, None))
            .hover(|style| style.bg(theme::selected()).border_color(theme::border_strong()))
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .child(div().flex_1().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(if stored.is_some() && !gone { theme::text() } else { theme::text_soft() }).child(shown))
            .child(glyph("icons/chevron-down.svg", 14., theme::text_soft()))
            .child(canvas(move |bounds, _, _| { anchors.borrow_mut().insert(anchor, bounds); }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.picker == Some(picker) {
                    this.close_picker(window, cx);
                } else {
                    this.open_picker(picker, window, cx);
                }
            }));
        let reset = stored.as_ref().map(|_| {
            reset_button(SharedString::from(format!("{id}-reset")), format!("Reset {label} to the system default").into())
                .on_click(cx.listener(move |this, _, _, cx| this.set(id, GLOBAL.into(), key, None, cx)))
        });
        row(label, second, div().flex().items_center().child(reset_slot(reset)).child(picker_button))
    }

    /// A picker's choices in menu order, by the value stored and the words shown: the system
    /// default first, then every device listed, then the stored one if it is not connected.
    pub(super) fn picker_choices(&self, picker: Picker) -> Vec<(Option<String>, String)> {
        let devices = self.devices.clone().unwrap_or_default();
        let (listed, default, key) = match picker {
            Picker::Input => (devices.inputs, devices.default_input, INPUT),
            Picker::Output => (devices.outputs, devices.default_output, OUTPUT),
        };
        let system = if default.is_empty() { "System default".to_owned() } else { format!("System default ({default})") };
        let mut choices = vec![(None, system)];
        let stored = prefs::values().device(key);
        let missing = stored.filter(|name| !listed.contains(name));
        choices.extend(listed.into_iter().map(|name| (Some(name.clone()), name)));
        choices.extend(missing.map(|name| (Some(name.clone()), format!("{name} (not connected)"))));
        choices
    }

    pub(super) fn device_menu(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (key, anchor) = match picker {
            Picker::Input => (INPUT, "V1-menu"),
            Picker::Output => (OUTPUT, "V2-menu"),
        };
        let bounds = self.anchors.borrow().get(anchor).copied()?;
        let stored = prefs::values().device(key);
        let choices = self.picker_choices(picker);
        let cursor = self.menu_cursor.min(choices.len().saturating_sub(1));
        // As wide as the system's names, up to 440, so two devices that differ only at the end
        // can be told apart; never narrower than the button.
        let mut items = menu().min_w(px(bounds.size.width.into())).max_w(px(440.));
        for (ix, (value, words)) in choices.into_iter().enumerate() {
            if ix == 1 {
                items = items.child(menu_separator());
            }
            let item = menu_item_check(("device", ix), stored == value, words, None)
                .when(ix == cursor, |item| item.bg(theme::selected()))
                .on_click(cx.listener(move |this, _, window, cx| this.pick_device(picker, value.clone(), window, cx)));
            items = items.child(item);
        }
        let menu = div()
            .track_focus(&self.menu_focus)
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                if this.menu_key(picker, &event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picker = None;
                cx.notify();
            }))
            .child(items);
        Some(placed("device-menu", Place::Below(bounds), Layer::Menu, window, menu))
    }

    /// V3 and V6 on one row: the live meter with its Test button, and under it the
    /// sensitivity slider whose thumb sits under the meter's line.
    fn meter_row(&mut self, cx: &mut Context<Self>) -> Div {
        let gate = self.pending.get("V6").copied().unwrap_or_else(|| prefs::values().number(SENSITIVITY));
        let in_call = self.shell.upgrade().and_then(|shell| shell.read(cx).snapshot.as_ref()?.call.clone());
        let level = match (&in_call, self.testing) {
            (Some(call), Some(_)) => call.level,
            (None, Some(_)) => self.level,
            _ => None,
        };
        let testing = self.testing.is_some();
        let test = button("V3", ButtonKind::Secondary, if testing { "Stop" } else { "Test" })
            .w(px(controls::VALUE_BOX + controls::VALUE_GAP - 8.))
            .on_click(cx.listener(|this, _, _, cx| {
                if this.reachable(cx) {
                    this.toggle_test(cx);
                }
            }));
        let push = prefs::values().on(PUSH_TO_TALK);
        let second = match (testing, push) {
            (true, false) => "Speak normally. Green is what others would hear.",
            (true, true) => "Speak normally. In push to talk the key opens your microphone, not the line.",
            (false, false) => "Below the line nothing is sent and your speaking ring stays off.",
            (false, true) => "In push to talk the line does nothing; it is used when you go back to voice activation.",
        };
        let label = |words: &'static str| div().w(px(96.)).type_ui().text_color(theme::text()).child(words);
        let meter = div()
            .id("V3-meter")
            .role(gpui::Role::Meter)
            .aria_label("Input level")
            .aria_numeric_value(f64::from(level.unwrap_or(0)))
            .child(controls::meter(level, gate));
        div()
            .px(px(12.))
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().h(px(28.)).flex().items_center().child(label("Input level")).child(div().flex_1()).child(div().w(px(controls::RESET_SLOT))).child(meter).child(div().w(px(8.))).child(test))
            // The value box and its gap are as wide as the Test button and its gap: the track
            // sits under the meter and the value ends where every other value does.
            .child(div().flex().items_center().child(label("Sensitivity")).child(div().flex_1()).child(self.slider_control("V6", cx)))
            .child(div().pt(px(2.)).type_small().text_color(theme::text_soft()).child(second))
    }

    /// V13, V15 to V17: a key recorder (design 3.8). Click, then press a key, a key with
    /// modifiers, a modifier alone, or the middle or a side mouse button; Esc cancels and ⌫
    /// clears. A key another action has is refused in words and the old one stays.
    fn key_row(&mut self, id: &'static str, cx: &mut Context<Self>) -> Div {
        let label = label_of(id);
        let Some(action) = key_action(id) else {
            return div();
        };
        let bound = prefs::values().hotkey(action.key());
        let recording = self.recording == Some(id);
        let shown: AnyElement = match (recording, bound) {
            (true, _) => div().type_ui().text_color(theme::text()).child("Press a key…").into_any_element(),
            (false, Some(binding)) => keycap(binding.label()).into_any_element(),
            (false, None) => div().type_ui().text_color(theme::text_faint()).child("Click to record").into_any_element(),
        };
        let mut field = div()
            .id(SharedString::from(format!("{id}-key")))
            .w(px(140.))
            .h(px(28.))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .border_1()
            .border_color(if recording { theme::accent() } else { theme::border() })
            .flex()
            .items_center()
            .cursor_pointer()
            .tab_index(0)
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("{label}: {}", bound.map_or("none".to_owned(), |b| b.label()))))
            .focus_visible(|style| style.shadow(theme::focus_ring()))
            .child(shown)
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.recording == Some(id) {
                    this.stop_recording(cx);
                } else {
                    this.start_recording(id, window, cx);
                }
            }));
        if recording {
            field = field
                .track_focus(&self.recorder_focus)
                .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                    if this.recording != Some(id) {
                        return;
                    }
                    cx.stop_propagation();
                    match event.keystroke.key.as_str() {
                        "escape" => this.stop_recording(cx),
                        "backspace" if !event.keystroke.modifiers.modified() => this.recorded(None, cx),
                        _ => {
                            if let Some(binding) = hotkeys::binding_of(&event.keystroke) {
                                this.recorded(Some(binding), cx);
                            }
                        }
                    }
                }))
                .on_modifiers_changed(cx.listener(move |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                    if this.recording != Some(id) {
                        return;
                    }
                    let now = event.modifiers;
                    if now.modified() {
                        let held = &mut this.held_modifiers;
                        held.control |= now.control;
                        held.alt |= now.alt;
                        held.shift |= now.shift;
                        held.platform |= now.platform;
                    } else {
                        // Let go with no other key: the modifier alone is the key.
                        let alone = hotkeys::modifier_binding(std::mem::take(&mut this.held_modifiers));
                        if alone.is_some() {
                            this.recorded(alone, cx);
                        }
                    }
                }));
        }
        let reset = bound.map(|_| {
            reset_button(SharedString::from(format!("{id}-reset")), format!("Clear {label}").into()).on_click(cx.listener(move |this, _, _, cx| {
                this.stop_recording(cx);
                this.set(id, GLOBAL.into(), action.key(), None, cx);
            }))
        });
        let side = bound.is_some_and(|b| matches!(b.key, protocol::hotkey::Key::Mouse4 | protocol::hotkey::Key::Mouse5)) && !hotkeys::side_buttons();
        let second = match action {
            Action::Priority => Some("Push to talk that also asks to be heard over everyone, where you hold priority speech."),
            _ if side => Some("This system doesn't show the mouse's side buttons to apps. Pick a key."),
            _ => None,
        };
        let clash = self.clash.as_ref().filter(|(row, _)| *row == id).map(|(_, words)| div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::danger()).child(words.clone()));
        let failed = self.failed.filter(|row| *row == id).map(|row| div().px(px(12.)).pb(px(8.)).type_small().text_color(theme::danger()).child(failure_words(row, self.reachable(cx))));
        let waiting = self.waits_on_chatd(id, cx);
        let lit = self.highlight == Some(id);
        div()
            .flex()
            .flex_col()
            .rounded(px(theme::R_CARD))
            .when(lit, |row| row.bg(theme::selected()))
            .when(waiting, |row| row.opacity(0.4))
            .child(row(label, second.map(SharedString::from), div().flex().items_center().child(reset_slot(reset)).child(field)))
            .children(clash)
            .children(failed)
    }

    /// D1: what tinyclient keeps on this Mac, as a two-part bar.
    fn usage(&mut self, cx: &mut Context<Self>) -> Div {
        let Some((messages, files)) = self.storage else {
            return row("Storage used", None, div().type_ui().text_color(theme::text_soft()).child("Counting…"));
        };
        let (message_words, file_words, total_words) = usage_words(messages, files);
        let total = (messages + files).max(1);
        let width = 238.;
        let first = (messages as f32 / total as f32 * width).round();
        let legend = |color: gpui::Rgba, words: String| div().flex().items_center().gap(px(6.)).child(div().size(px(8.)).rounded(px(2.)).bg(color)).child(words);
        div()
            .px(px(12.))
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(crate::host::on_here(&total_words)))
                    .child(button("D1", ButtonKind::Ghost, crate::host::REVEAL).on_click(cx.listener(|_, _, _, cx| cx.open_with_system(&protocol::support_dir())))),
            )
            .child(
                div()
                    .w(px(width))
                    .h(px(8.))
                    .rounded(px(4.))
                    .overflow_hidden()
                    .flex()
                    .gap(px(1.))
                    .bg(theme::raised())
                    .child(div().w(px(first)).h_full().bg(theme::accent()))
                    .child(div().flex_1().h_full().bg(theme::positive())),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.))
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(legend(theme::accent(), format!("Messages {message_words}")))
                    .child(legend(theme::positive(), format!("Images and files {file_words}"))),
            )
    }

    /// P4: a longer time applies at once; a shorter one asks first, with what it deletes.
    fn pick_retention(&mut self, days: u32, cx: &mut Context<Self>) {
        if !self.reachable(cx) {
            return;
        }
        if days >= prefs::values().number(KEEP_DAYS) {
            let stored = (days != Values::default_number(KEEP_DAYS)).then(|| days.to_string());
            self.set("P4", GLOBAL.into(), KEEP_DAYS, stored, cx);
            return;
        }
        self.confirm = Some(Confirm::Retention { days, prunable: None });
        self.off_thread(cx, move || crate::client::ask(&Request::Prunable { days }), move |this, answer, cx| {
            if let (Some(Confirm::Retention { days: asked, prunable }), Ok(Response::Prunable { messages, bytes })) = (&mut this.confirm, answer) {
                if *asked == days {
                    *prunable = Some((messages, bytes));
                    cx.notify();
                }
            }
        });
        cx.notify();
    }

    pub(super) fn modal(&mut self, confirm: &Confirm, cx: &mut Context<Self>) -> Div {
        match confirm.clone() {
            Confirm::SignOut(account) => self.sign_out_modal(&account, cx),
            Confirm::TinyEverywhere(account) => self.tiny_everywhere_modal(&account, cx),
            Confirm::Retention { days, prunable } => modal_panel(
                format!("Delete messages older than {days} days?"),
                vec![retention_words(prunable)],
                button("retention-confirm", ButtonKind::Danger, "Delete").on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm = None;
                    this.set("P4", GLOBAL.into(), KEEP_DAYS, Some(days.to_string()), cx);
                })),
                cx,
            ),
            Confirm::Clear => modal_panel(
                "Clear saved messages and files?",
                vec![crate::host::clear_saved()],
                button("clear-confirm", ButtonKind::Danger, "Clear").on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    this.off_thread(cx, || crate::client::ask(&Request::Clear), |this, answer, cx| {
                        if !matches!(answer, Ok(Response::Ok)) {
                            this.failed = Some("D3");
                        }
                        this.read_storage(cx);
                        cx.notify();
                    });
                    cx.notify();
                })),
                cx,
            ),
            Confirm::Reset => modal_panel(
                "Reset every setting to its default?",
                vec!["Accounts, messages and files are not touched.".to_owned()],
                button("reset-confirm", ButtonKind::Danger, "Reset").on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    this.reset_all(cx);
                })),
                cx,
            ),
        }
    }

    /// B3: chatd's settings, the window's own, and the text size, all to their defaults.
    fn reset_all(&mut self, cx: &mut Context<Self>) {
        let section = prefs::choices().section;
        prefs::set_choices(Choices { section, ..Choices::default() });
        theme::set_text_step(0);
        anim::set_autoplay(Autoplay::default());
        self.pending.clear();
        let _ = self.shell.update(cx, |shell, cx| {
            shell.save_prefs(cx);
            cx.notify();
        });
        cx.refresh_windows();
        self.off_thread(cx, || crate::client::ask(&Request::ResetSettings), |this, answer, cx| {
            if !matches!(answer, Ok(Response::Ok)) {
                this.failed = Some("B3");
            }
            let _ = this.shell.update(cx, |shell, cx| shell.load_settings(cx));
            cx.notify();
        });
    }

    fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let text = shell.read(cx).diagnostics();
        self.off_thread(cx, move || super::super::app::with_memory(text), |this, text, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            this.copied = Some(std::time::Instant::now());
            cx.notify();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        });
    }

    /// The Shortcuts section, or its lines that match `query`.
    pub(super) fn shortcuts(&self, query: &str) -> Div {
        let mut column = div().flex().flex_col();
        if query.is_empty() {
            column = column.child(caption(crate::host::SHORTCUTS_CAPTION).pt(px(0.)).pb(px(14.)));
        }
        let found = rows::shortcuts(query);
        let mut menus: Vec<&str> = Vec::new();
        for shortcut in &found {
            if !menus.contains(&shortcut.menu) {
                menus.push(shortcut.menu);
            }
        }
        for (ix, name) in menus.iter().enumerate() {
            let mut card = card();
            for (n, shortcut) in found.iter().filter(|s| s.menu == *name).enumerate() {
                let keys = div().flex().gap(px(2.)).child(keycap(shortcut.keys));
                card = card.when(n > 0, |card| card.child(rule())).child(
                    div().h(px(32.)).px(px(12.)).flex().items_center().child(div().flex_1().type_ui().text_color(theme::text()).child(shortcut.command)).child(keys),
                );
            }
            // In search results each group says where it is, as every other result does.
            let label = if query.is_empty() {
                group_label(*name)
            } else {
                div().pl(px(12.)).pb(px(4.)).type_small().text_color(theme::text_faint()).child(format!("Shortcuts › {name}"))
            };
            let gap = if query.is_empty() { 20. } else { 12. };
            column = column.child(div().when(ix > 0, |group| group.pt(px(gap))).flex().flex_col().child(label).child(card));
        }
        column
    }

    fn update_status(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let (words, label) = match crate::update::shown() {
            crate::update::Shown::Ready { version } => (format!("Version {version} is ready. Restart to install it."), Some("Restart now")),
            crate::update::Shown::Available { version, .. } => {
                let label = if crate::update::install_into().is_some() { "Install" } else { "Download" };
                (format!("Version {version} is available."), Some(label))
            }
            crate::update::Shown::Failed { version } => (format!("Couldn't install {version}. Will try again tomorrow."), None),
            crate::update::Shown::Quiet => return None,
        };
        let action = label.map(|label| {
            button("about-update", ButtonKind::Ghost, label).h(px(24.)).on_click(cx.listener(|this, _, _, cx| this.install_shown(cx)))
        });
        Some(
            div()
                .pt(px(4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().type_small().text_color(theme::text_soft()).child(words))
                .children(action),
        )
    }

    fn install_shown(&mut self, cx: &mut Context<Self>) {
        match crate::update::shown() {
            crate::update::Shown::Ready { .. } => cx.quit(),
            crate::update::Shown::Available { page, .. } => {
                if crate::update::install_into().is_some() {
                    self.off_thread(cx, || crate::update::pursue(crate::update::Policy::Install), |this, _, cx| {
                        let _ = this.shell.update(cx, |shell, cx| {
                            shell.layout_state.update_alert = true;
                            cx.notify();
                        });
                        cx.notify();
                    });
                } else {
                    cx.open_url(&page);
                }
            }
            _ => {}
        }
    }

    fn about(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // The commit tells two builds of the same version apart.
        let version = match crate::update::short_commit() {
            Some(commit) => format!("Version {} ({commit})", env!("CARGO_PKG_VERSION")),
            None => format!("Version {}", env!("CARGO_PKG_VERSION")),
        };
        let plate = div()
            .size(px(96.))
            .rounded(px(22.))
            .bg(gpui::rgb(0x1a1c2a))
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .justify_center()
            .child(glyph("icons/messages-square.svg", 52., gpui::rgb(0xa3acff)));
        let credit = |words: &'static str| div().px(px(12.)).py(px(10.)).type_small().text_color(theme::text_soft()).child(words);
        let tools = self.group(&["B1", "B2", "B3"], None, window, cx);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .child(plate)
            .child(div().pt(px(10.)).type_headline().text_color(theme::text()).child("tinyclient"))
            .child(div().type_small().text_color(theme::text_faint()).child(version))
            .children(self.update_status(cx))
            .child(div().type_ui().text_color(theme::text_soft()).child("One small app for Slack, Discord, Telegram, Teams and Signal."))
            .child(
                div()
                    .w_full()
                    .pt(px(18.))
                    .flex()
                    .flex_col()
                    .child(group_label("Acknowledgements"))
                    .child(
                        card()
                            .child(credit("Network icons: Dashboard Icons (homarr-labs/dashboard-icons), Apache-2.0. The logos are trademarks of their owners."))
                            .child(rule())
                            .child(credit("Interface icons: Lucide, ISC. Sounds: made by tinyclient's own code."))
                    ),
            )
            .child(div().w_full().pt(px(20.)).child(tools))
    }

    /// Search results: each a breadcrumb and its live row, best first; or the honest line.
    pub(super) fn results(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let query = self.query.clone();
        let found: Vec<&'static Row> = rows::search(&query);
        let keys = rows::shortcuts(&query);
        let mut column = div().w(px(controls::COLUMN)).flex().flex_col().gap(px(12.));
        if found.is_empty() && (keys.is_empty() || query.trim().len() < 2) {
            let honest = rows::not_yet(&query);
            return column.child(
                div()
                    .pt(px(80.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph("icons/search.svg", 24., theme::text_faint()))
                    .child(div().type_title().text_color(theme::text()).child(format!("No settings match “{}”.", query.trim())))
                    .children(honest.map(|line| div().type_ui().text_color(theme::text_soft()).child(line))),
            );
        }
        for row in found {
            let breadcrumb = div()
                .id(SharedString::from(format!("crumb-{}", row.id)))
                .pl(px(12.))
                .cursor_pointer()
                .type_small()
                .text_color(theme::text_faint())
                .hover(|style| style.text_color(theme::text_soft()))
                .child(rows::crumb(row))
                .on_click(cx.listener(move |this, _, window, cx| this.reveal(row, window, cx)));
            // P1 and P2 are their own card of accounts; every other row sits in one.
            let live = if rows::draws_label(row.id) { card().child(self.control_row(row.id, window, cx)) } else { self.control_row(row.id, window, cx) };
            column = column.child(div().flex().flex_col().gap(px(4.)).child(breadcrumb).child(live));
        }
        if !keys.is_empty() && query.trim().len() >= 2 {
            column = column.child(self.shortcuts(&query));
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retention_question_says_how_much_goes_and_drops_what_it_does_not_know() {
        let here = crate::host::HERE;
        assert_eq!(
            retention_words(Some((41_210, 12 * 1024 * 1024))),
            format!("41,210 messages, about 12 MB, are removed from {here}. They stay on Slack, Discord and the others.")
        );
        assert_eq!(retention_words(Some((1, 300))), format!("1 message is removed from {here}. It stays on Slack, Discord and the others."));
        assert_eq!(retention_words(None), format!("Older messages are removed from {here}. They stay on Slack, Discord and the others."));
    }

    #[test]
    fn the_storage_legend_adds_up_to_the_heading() {
        let mb = |tenths: u64| tenths * 1024 * 1024 / 10;
        // 5.24 MB and 2.34 MB were shown as 5.2 + 2.3 under a heading of 7.6.
        assert_eq!(usage_words(mb(52) + 40_000, mb(23) + 40_000), ("5.2 MB".into(), "2.3 MB".into(), "7.5 MB".into()));
        assert_eq!(usage_words(mb(52), mb(73)), ("5 MB".into(), "7 MB".into(), "12 MB".into()));
        assert_eq!(usage_words(1_000, mb(30)), ("under 0.1 MB".into(), "3.0 MB".into(), "3.0 MB".into()));
        assert_eq!(usage_words(mb(3_000), mb(20_000)), ("0.3 GB".into(), "2.0 GB".into(), "2.3 GB".into()));
        assert_eq!(usage_words(0, 0).2, "0.0 MB");
    }

    #[test]
    fn every_slider_stores_only_what_differs_from_its_default() {
        for id in ["S10", "V5", "V6"] {
            let spec = slider(id);
            assert!(spec.range.0 <= spec.default && spec.default <= spec.range.1, "{id}");
        }
        assert_eq!(slider("A2").default, 1, "text size 14 pt is the second stop");
        assert_eq!(slider("V6").default, Values::default_number(SENSITIVITY));
        assert_eq!(slider("S10").default, Values::default_number(CUE_VOLUME));
        assert_eq!(slider("V5").default, Values::default_number(VOICE_VOLUME));
    }
}
