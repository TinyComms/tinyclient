//! How each row is drawn. No bubbles for anyone: avatar, name, time, body, the same on every
//! network. Every box that holds a picture is sized from what the network said about it, so a
//! row is its final height before any file arrives.

use palette::IntoColor;
use super::actions::reactor_words;
use super::emoji;
use super::icons::{icon, Icon};
use super::images::Picture;
use super::media::{self, Fit, Shape as MediaShape};
use super::rich::{self, Block, Inline, Jumbo, RunAction};
use super::rows::Item;
use super::text::{Look, Rich, TextEvent};
use super::tokens::{self, Metrics, MEASURE};
use super::{edited_tip, Pane, PaneEvent, Selection, Then};
use crate::gui::anim::{self, Play};
use crate::gui::components::{button, glyph, sensitive_cover, tip, Avatar, ButtonKind, Shape};
use crate::gui::phone::tokens as tokens_phone;
use crate::gui::theme::{self, Type};
use crate::gui::time;
use crate::view::now;
use crate::client::ask;
use gpui::{
    anchored, canvas, deferred, div, prelude::*, px, Animation, AnimationExt, AnyElement, ClipboardItem, Context, Div, ElementId,
    FontWeight, HighlightStyle, Hsla, MouseButton, ObjectFit, Pixels, Point, RenderImage, SharedString, Stateful, StyledText, Window,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use protocol::{Attachment, Card, Forwarded, InviteState, Key, MediaFile, MentionKind, Message, MessageInvite, MessageKind, Network, Preview, Receipt, Request, Response, ScheduledEvent, SendState, Via};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// How long the tint on a jump's target takes to fade.
pub const FLASH: Duration = Duration::from_millis(1200);

#[test]
fn translation_attribution_only_claims_a_known_source() {
    let mut translation = protocol::Translation {
        text: "See you tomorrow.".into(),
        source_lang: Some("es".into()),
        target_lang: "en".into(),
    };
    assert_eq!(translation_attribution(&translation), "Translated from Spanish");
    for source in [None, Some("auto"), Some("und"), Some(" ")] {
        translation.source_lang = source.map(str::to_owned);
        assert_eq!(translation_attribution(&translation), "Translated to English");
    }
}

fn translation_attribution(translation: &protocol::Translation) -> String {
    let source = translation.source_lang.as_deref().map(str::trim).filter(|tag| {
        let primary = tag.split(['-', '_']).next().unwrap_or_default();
        !primary.is_empty() && !["auto", "und", "mul", "zxx"].iter().any(|code| primary.eq_ignore_ascii_case(code))
    });
    match source {
        Some(tag) => format!("Translated from {}", protocol::language_name(tag)),
        None => format!("Translated to {}", protocol::language_name(&translation.target_lang)),
    }
}

fn message_label(message: &Message, hidden: bool) -> String {
    let sender = if message.outgoing { "You" } else if message.sender.name.is_empty() {
        "Unknown sender"
    } else { &message.sender.name };
    let mut parts = vec![sender.to_owned(), short_clock(message.sent)];
    if message.deleted {
        parts.push("Message deleted".into());
    } else if hidden {
        parts.push("Message hidden".into());
    } else {
        if message.spans.iter().any(|span| matches!(span.style, protocol::Style::Spoiler)) {
            parts.push("Message contains a spoiler".into());
        } else if !message.text.trim().is_empty() {
            parts.push(message.text.clone());
        }
        parts.extend(message.attachments.iter().map(|file| {
            if file.spoiler || file.view_once { "Hidden attachment".into() }
            else if !file.alt.is_empty() { format!("Attachment: {}", file.alt) }
            else if !file.name.is_empty() { format!("Attachment: {}", file.name) }
            else { "Attachment".into() }
        }));
        parts.extend(message.reactions.iter().map(|reaction| format!("{} {}", reaction.count, reaction.emoji)));
    }
    parts.join(", ")
}

#[test]
fn a_hidden_message_label_does_not_include_its_contents() {
    let message = Message::plain("m1", "Nadia", "Private words", 0, false);
    assert!(message_label(&message, false).contains("Private words"));
    let hidden = message_label(&message, true);
    assert!(hidden.starts_with("Nadia, "));
    assert!(hidden.ends_with("Message hidden"));
    assert!(!hidden.contains("Private words"));
}

fn poll_percentage(count: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    let count = u64::from(count.min(total));
    let total = u64::from(total);
    ((100 * count + total / 2) / total) as u32
}

#[cfg(test)]
#[test]
fn poll_result_percentages_round_and_stay_within_the_card() {
    assert_eq!(poll_percentage(0, 0), 0);
    assert_eq!(poll_percentage(2, 3), 67);
    assert_eq!(poll_percentage(u32::MAX, u32::MAX), 100);
    assert_eq!(poll_percentage(5, 2), 100);
}

/// The line under a send that is queued until the account is back.
pub(crate) fn waiting_line(network: &str) -> String {
    format!("Waiting for {network}")
}
/// Lines of a code block shown before "Show more".
const CODE_LINES: usize = 20;

fn ink_style(ink: super::highlight::Ink) -> HighlightStyle {
    let paint = |color: gpui::Rgba| Some(color.into_color());
    match ink {
        super::highlight::Ink::Keyword => HighlightStyle { color: paint(tokens::code_keyword()), ..Default::default() },
        super::highlight::Ink::String => HighlightStyle { color: paint(tokens::code_string()), ..Default::default() },
        super::highlight::Ink::Comment => HighlightStyle { color: paint(tokens::code_comment()), ..Default::default() },
        super::highlight::Ink::Number => HighlightStyle { color: paint(tokens::code_number()), ..Default::default() },
        super::highlight::Ink::Type => HighlightStyle { color: paint(tokens::code_type()), ..Default::default() },
        super::highlight::Ink::Function => HighlightStyle { color: paint(tokens::code_function()), ..Default::default() },
        super::highlight::Ink::Added => HighlightStyle {
            color: paint(tokens::code_added()),
            background_color: paint(tokens::code_added_bg()),
            ..Default::default()
        },
        super::highlight::Ink::Removed => HighlightStyle {
            color: paint(tokens::code_removed()),
            background_color: paint(tokens::code_removed_bg()),
            ..Default::default()
        },
        super::highlight::Ink::Ansi { fg, bg, bold } => HighlightStyle {
            color: tokens::code_ansi(fg).map(|color| color.into_color()),
            background_color: tokens::code_ansi(bg).map(|color| color.into_color()),
            font_weight: bold.then_some(FontWeight::BOLD),
            ..Default::default()
        },
    }
}

/// The first `count` lines, as a byte prefix, so colour ranges stay on the text that is drawn.
fn first_lines(text: &str, count: usize) -> &str {
    let mut lines = 0;
    for (at, _) in text.match_indices('\n') {
        lines += 1;
        if lines == count {
            return &text[..at];
        }
    }
    text.trim_end_matches(['\n', '\r'])
}

fn code_highlights(text: &str, runs: &[super::highlight::Paint]) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut highlights = Vec::new();
    for run in runs {
        let start = run.start as usize;
        let end = (run.end as usize).min(text.len());
        if start < end && text.is_char_boundary(start) && text.is_char_boundary(end) {
            highlights.push((start..end, ink_style(run.ink)));
        }
    }
    highlights
}
/// Link cards drawn per message; further links stay text.
const CARDS: usize = 3;
/// Reaction chips drawn before the rest are counted.
const CHIPS: usize = 12;

/// Where files are linked under their own names to be opened: `open/` in the support
/// directory, emptied at each start. The cache names files by a hash with no extension, which
/// no player, Quick Look or default app can tell the type of.
fn opened_dir() -> std::path::PathBuf {
    protocol::support_dir().join("open")
}

/// Empties [`opened_dir`], once a run. A player that still has a file open keeps it: the
/// link goes, the bytes stay until it closes them.
fn clear_opened() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| drop(std::fs::remove_dir_all(opened_dir())));
}

/// The cached file at `path` under `name`: a hard link in [`opened_dir`], so no bytes are
/// copied; a copy where a link cannot be made; the file itself if neither works.
fn named(path: &str, name: &str) -> std::path::PathBuf {
    clear_opened();
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    let dir = opened_dir().join(format!("{hash:016x}"));
    let target = dir.join(name);
    if target.exists() {
        return target;
    }
    let made = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::hard_link(path, &target).or_else(|_| std::fs::copy(path, &target).map(drop)));
    match made {
        Ok(()) => {
            // Opened by the system's app: marked as from the internet first, so it is checked.
            if let Err(err) = protocol::quarantine::mark(&target) {
                tracing::warn!("tinyclient: a file was not marked as downloaded: {err}");
            }
            target
        }
        Err(err) => {
            tracing::warn!("tinyclient: could not name {name} for opening: {err}");
            std::path::PathBuf::from(path)
        }
    }
}

/// Opens a cached file under its own name: in Quick Look, or in the system's app for it (a
/// video in the system's player). Both are processes of their own; a thread waits for them so
/// nothing of ours blocks, and makes the link off the main thread.
pub fn open_file(path: &str, name: &str, then: Then, cx: &mut gpui::App) {
    // A phone has no xdg-open: the file goes to GPUI's URL open, on this thread.
    if super::super::links_through_gpui() {
        let _ = then;
        cx.open_url(&format!("file://{}", named(path, name).display()));
        return;
    }
    let (path, name) = (path.to_owned(), name.to_owned());
    std::thread::spawn(move || {
        let path = named(&path, &name);
        let mut command;
        #[cfg(target_os = "macos")]
        {
            command = match then {
                Then::QuickLook => Command::new("/usr/bin/qlmanage"),
                Then::Open => Command::new("/usr/bin/open"),
            };
            if then == Then::QuickLook {
                command.arg("-p");
            }
            command.arg(&path);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = then;
            command = Command::new("xdg-open");
            command.arg(&path);
        }
        #[cfg(windows)]
        {
            let _ = then;
            command = Command::new("cmd");
            command.arg("/C").arg("start").arg("").arg(&path);
            protocol::launch::no_console(&mut command);
        }
        let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
    });
}

fn name_id(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("{prefix}-{id}")))
}

/// `9:07`, for the gutter of a continuation row, which is too narrow for the half of the day.
fn short_clock(unix: i64) -> String {
    let clock = time::clock(unix);
    clock.split(' ').next().unwrap_or_default().to_owned()
}

fn day_line(at: i64, metrics: Metrics) -> Div {
    let rule = || div().flex_1().h(px(1.)).bg(theme::hairline());
    let label = time::divider(now(), at);
    if metrics == Metrics::PHONE {
        // 24 above, 12 below, a 12/16 label between two hairlines.
        return div()
            .w_full()
            .pt(px(24.))
            .pb(px(12.))
            .px(px(metrics.inset))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(rule())
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens_phone::scaled(12.)))
                    .line_height(px(tokens_phone::scaled(16.)))
                    .font_weight(theme::MEDIUM)
                    .text_color(theme::text_faint())
                    .child(label),
            )
            .child(rule());
    }
    div()
        .pt(px(12.))
        .pb(px(4.))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(rule())
        .child(div().flex_none().type_micro().font_weight(theme::MEDIUM).text_color(theme::text_faint()).child(label))
        .child(rule())
}

/// The line above the first unread message: an accent rule anchored with a compact "New" pill.
fn new_line(metrics: Metrics) -> Div {
    let phone = metrics == Metrics::PHONE;
    div()
        .w_full()
        .when(phone, |line| line.pt(px(12.)).pb(px(6.)))
        .when(!phone, |line| line.pt(px(8.)).pb(px(4.)))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().flex_1().h(px(if phone { 2. } else { 1. })).rounded(px(1.)).bg(theme::accent()))
        .child(
            div()
                .flex_none()
                .px(px(6.))
                .py(px(1.))
                .rounded_full()
                .bg(theme::accent_soft())
                .text_size(if phone { px(tokens_phone::scaled(12.)) } else { theme::sized(11.) })
                .line_height(if phone { px(tokens_phone::scaled(16.)) } else { theme::line(14.) })
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("New"),
        )
}

fn replies_line(count: u32, metrics: Metrics) -> Div {
    let words = match count {
        0 => "No replies yet".to_owned(),
        1 => "1 reply".to_owned(),
        count => format!("{count} replies"),
    };
    div()
        .h(px(32.))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().type_micro().text_color(theme::text_faint()).child(words))
        .child(div().flex_1().h(px(1.)).bg(theme::hairline()))
}

fn system_line_div(pane: &Pane, message: &Message, cx: &mut Context<Pane>) -> Div {
    let _ = pane;
    let text_el = if let Some(target) = message.jump.clone().filter(|id| !id.is_empty()) {
        div()
            .id(SharedString::from(format!("sys-jump-{}", message.id)))
            .max_w(px(MEASURE))
            .text_center()
            .whitespace_normal()
            .cursor_pointer()
            .hover(|style| style.text_color(theme::text()).underline())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.jump_to(&target, cx)))
            .child(message.text.clone())
            .into_any_element()
    } else {
        div().max_w(px(MEASURE)).text_center().whitespace_normal().child(message.text.clone()).into_any_element()
    };
    let quote_el = message.reply_to.as_ref().map(|reply| {
        let target = message.jump.as_ref().filter(|id| !id.is_empty()).cloned();
        let snippet = if reply.deleted {
            "Original message was deleted".to_owned()
        } else if reply.sender.is_empty() {
            reply.snippet.clone()
        } else {
            format!("{}: {}", reply.sender, reply.snippet)
        };
        let row = div()
            .id(SharedString::from(format!("sys-quote-{}", message.id)))
            .mt(px(3.))
            .max_w(px(MEASURE))
            .pl(px(8.))
            .border_l_2()
            .border_color(theme::hairline())
            .type_small()
            .text_color(theme::text_soft())
            .truncate()
            .child(snippet);
        if let Some(target) = target {
            row.cursor_pointer()
                .hover(|style| style.text_color(theme::text()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(&target, cx)))
        } else {
            row
        }
    });
    div()
        .min_h(px(28.))
        .px(px(20.))
        .py(px(5.))
        .flex()
        .flex_col()
        .items_center()
        .type_small()
        .text_color(theme::text_faint())
        .child(text_el)
        .children(quote_el)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallRowSpec {
    pub icon: &'static str,
    pub missed: bool,
    pub label: String,
    pub pill: Option<&'static str>,
}

fn format_call_duration(seconds: u32) -> String {
    if seconds >= 3600 {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        if m == 0 { format!("{h} hr") } else { format!("{h} hr {m} min") }
    } else if seconds >= 60 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} sec")
    }
}

pub(crate) fn call_line_spec(message: &Message, peer_title: &str) -> CallRowSpec {
    let other = if message.outgoing {
        if !peer_title.is_empty() { peer_title } else { "Someone" }
    } else if !message.sender.name.is_empty() {
        message.sender.name.as_str()
    } else if !peer_title.is_empty() {
        peer_title
    } else {
        "Someone"
    };
    let Some(note) = &message.call else {
        return CallRowSpec {
            icon: "icons/phone.svg",
            missed: false,
            label: message.text.clone(),
            pill: None,
        };
    };
    let video = note.video;
    let noun = if video { "video call" } else { "call" };
    let cap_noun = if video { "Video call" } else { "Call" };
    match (note.outcome, message.outgoing) {
        (protocol::CallOutcome::Missed, false) => CallRowSpec {
            icon: "icons/phone-missed.svg",
            missed: true,
            label: format!("Missed {noun} from {other}"),
            pill: Some("Call back"),
        },
        (protocol::CallOutcome::Missed, true) => CallRowSpec {
            icon: "icons/phone-outgoing.svg",
            missed: false,
            label: format!("You called {other} · no answer"),
            pill: Some("Call again"),
        },
        (protocol::CallOutcome::Declined, false) => CallRowSpec {
            icon: "icons/phone-off.svg",
            missed: false,
            label: format!("You declined a {noun} from {other}"),
            pill: None,
        },
        (protocol::CallOutcome::Declined, true) => CallRowSpec {
            icon: "icons/phone-off.svg",
            missed: false,
            label: format!("{other} declined the {noun}"),
            pill: Some("Call again"),
        },
        (protocol::CallOutcome::Cancelled, false) => CallRowSpec {
            icon: "icons/phone-off.svg",
            missed: false,
            label: format!("{other} cancelled the {noun}"),
            pill: None,
        },
        (protocol::CallOutcome::Cancelled, true) => CallRowSpec {
            icon: "icons/phone-off.svg",
            missed: false,
            label: format!("You cancelled the {noun}"),
            pill: Some("Call again"),
        },
        (protocol::CallOutcome::Busy, _) => CallRowSpec {
            icon: "icons/phone-off.svg",
            missed: false,
            label: format!("{other}'s line was busy"),
            pill: Some("Call again"),
        },
        (protocol::CallOutcome::Ended, _) => {
            let label = match note.seconds.filter(|&s| s > 0) {
                Some(secs) => format!("{cap_noun} with {other} · {}", format_call_duration(secs)),
                None => format!("{cap_noun} with {other}"),
            };
            CallRowSpec {
                icon: if video {
                    "icons/video.svg"
                } else if message.outgoing {
                    "icons/phone-outgoing.svg"
                } else {
                    "icons/phone-incoming.svg"
                },
                missed: false,
                label,
                pill: Some("Call back"),
            }
        }
        (protocol::CallOutcome::Live, _) => CallRowSpec {
            icon: if video { "icons/video.svg" } else { "icons/phone.svg" },
            missed: false,
            label: format!("{cap_noun} in progress with {other}"),
            pill: Some("Join"),
        },
    }
}

/// A glyph and a line of words in the middle of a media box.
fn in_box(which: Icon, words: Option<&'static str>) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .type_small()
        .text_color(theme::text_faint())
        .child(icon(which, 20., theme::text_faint()))
        .children(words)
}

/// The label over a still that says what a click will do.
pub(super) fn badge(words: String) -> Div {
    div()
        .absolute()
        .bottom(px(6.))
        .h(px(20.))
        .px(px(6.))
        .rounded(px(5.))
        .bg(tokens::badge())
        .flex()
        .items_center()
        .type_micro()
        .font_weight(theme::SEMIBOLD)
        .text_color(gpui::white())
        .child(words)
}

pub(super) fn play_disc() -> Div {
    div().absolute().inset_0().flex().items_center().justify_center().child(
        div()
            .size(px(44.))
            .rounded_full()
            .bg(tokens::badge())
            .flex()
            .items_center()
            .justify_center()
            .child(icon(Icon::Play, 18., gpui::white())),
    )
}

/// Formats Unix seconds as UTC `YYYY-MM-DD` (Howard Hinnant's civil-from-days algorithm).
fn format_ymd(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

/// A small square button inside a row or a bar.
fn tool(id: impl Into<ElementId>, which: Icon, size: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(size))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .child(icon(which, 16., theme::text_soft()))
}

impl Pane {
    pub(super) fn row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let metrics = self.metrics;
        let body = match &item {
            Item::Start => self.start_row(window, cx).into_any_element(),
            Item::Older => self.older_row(cx).into_any_element(),
            Item::Root(message) => self.message_row(message, true, true, window, cx),
            Item::Replies(count) => replies_line(*count, metrics).into_any_element(),
            Item::Day(at) => day_line(*at, metrics).into_any_element(),
            Item::New => new_line(metrics).into_any_element(),
            Item::Message { message, head } => self.message_row(message, *head, false, window, cx),
            Item::System(message) if message.call.is_some() => self.call_line(message, cx).into_any_element(),
            Item::System(message) => self.system_row(message, cx).into_any_element(),
        };
        let seen = self.seen.clone();
        let mark = canvas(move |bounds, _, _| seen.borrow_mut().push((ix, bounds)), |_, (), _, _| {});
        div()
            .relative()
            .w_full()
            .child(body)
            .child(mark.absolute().top_0().left_0().size_full())
            .into_any_element()
    }

    /// A grey service line, plus any https link the adapter attached (a gift-code redeem).
    fn system_row(&self, message: &Message, cx: &mut Context<Self>) -> Div {
        let mut line = self.automod_row(message, cx);
        for (idx, control) in message.controls.iter().enumerate() {
            let protocol::ControlKind::Button { url, .. } = &control.kind else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            let target = url.clone();
            let label = if control.label.is_empty() { "Open".to_owned() } else { control.label.clone() };
            line = line.child(
                div()
                    .id(SharedString::from(format!("sys-{}-{idx}", message.id)))
                    .ml(px(8.))
                    .cursor_pointer()
                    .text_color(theme::accent_text())
                    .hover(|style| style.underline())
                    .child(label)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_external_link(&target, window, cx);
                    })),
            );
        }
        line
    }

    /// The widest a picture may be: 400, or what the pane leaves beside the gutter.
    fn media_width(&self) -> f32 {
        // The list is laying rows out when this runs, so its width is the one noted after the
        // last draw.
        let pane = self.width;
        let room = pane - self.metrics.gutter() - self.metrics.inset;
        if pane <= 0. {
            self.metrics.media
        } else {
            self.metrics.media.min(room).max(120.).floor()
        }
    }

    /// AutoMod alerts are system rows. Their buttons are not drawn here, so the alert opens in Discord.
    fn automod_row(&self, message: &Message, cx: &mut Context<Self>) -> Div {
        let link = message
            .text
            .starts_with("AutoMod ")
            .then(|| message.permalink.clone())
            .filter(|link| link.starts_with("https://") || link.starts_with("http://"));
        let mut row = system_line_div(self, message, cx);
        if let Some(link) = link {
            let id = SharedString::from(format!("automod-{}", message.id));
            row = row.child(
                div()
                    .id(id)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&link)))
                    .child("Open in Discord"),
            );
        }
        row
    }

    fn call_line(&self, message: &Message, cx: &mut Context<Self>) -> Div {
        let peer_title = self.open.as_ref().map_or("", |open| open.title.as_str());
        let spec = call_line_spec(message, peer_title);
        let disc_bg = if spec.missed { theme::danger_soft() } else { theme::quiet_fill() };
        let disc_fg = if spec.missed { theme::danger() } else { theme::text_soft() };
        let label_fg = if spec.missed { theme::danger() } else { theme::text_soft() };
        let open_key = self.open.as_ref().map(|open| open.key.clone());
        let faces: Option<Div> = message.call.as_ref().filter(|note| !note.participants.is_empty()).and_then(|note| {
            let key = open_key.as_ref()?;
            let account = key.account();
            let shell = self.shell.upgrade()?;
            let hub = shell.read(cx).people_state.hub().clone();
            let shown: Vec<Div> = note
                .participants
                .iter()
                .take(4)
                .map(|id| {
                    let person_key = Key::new(&account, id);
                    let known = hub.person(&person_key);
                    let name = known.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| id.clone());
                    let picture = known
                        .and_then(|p| p.avatar)
                        .and_then(|file| hub.file(&account, &file, cx).path)
                        .map(std::path::PathBuf::from);
                    Avatar::new(&person_key, &name, 18.).image_at(picture).build()
                })
                .collect();
            Some(div().flex_none().flex().items_center().gap(px(3.)).children(shown))
        });
        let call_support = self.open.as_ref().map_or(protocol::CallSupport::None, |open| open.call);
        let rate_pill = message
            .call
            .as_ref()
            .filter(|note| note.outcome == protocol::CallOutcome::Ended)
            .and_then(|note| {
                let call_id = note.call_id.clone()?;
                let key = open_key.clone()?;
                let shell = self.shell.clone();
                let prompt = protocol::CallRatingPrompt {
                    conversation: key,
                    call_id,
                    video: note.video,
                    seconds: note.seconds,
                };
                Some(
                    div()
                        .id(SharedString::from(format!("call-row-rate-{}", message.id)))
                        .flex_none()
                        .h(px(24.))
                        .px(px(10.))
                        .rounded_full()
                        .bg(theme::quiet_fill())
                        .text_color(theme::text_soft())
                        .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .type_micro()
                        .font_weight(theme::SEMIBOLD)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let prompt = prompt.clone();
                            let _ = shell.update(cx, |shell, cx| shell.call_open_rating(prompt, true, cx));
                        }))
                        .child("Rate"),
                )
            });
        let shell = self.shell.clone();
        let pill = spec.pill.and_then(|words| {
            let key = open_key?;
            let handoff = match call_support {
                protocol::CallSupport::Native => false,
                protocol::CallSupport::Handoff => true,
                protocol::CallSupport::None | protocol::CallSupport::Restricted => return None,
            };
            Some(
                div()
                    .id(SharedString::from(format!("call-row-act-{}", message.id)))
                    .flex_none()
                    .h(px(24.))
                    .px(px(10.))
                    .rounded_full()
                    .bg(if spec.missed { theme::danger_soft() } else { theme::accent_soft() })
                    .text_color(if spec.missed { theme::danger() } else { theme::accent_text() })
                    .hover(|style| {
                        if spec.missed {
                            style.bg(theme::danger_fill()).text_color(theme::on_accent())
                        } else {
                            style.bg(theme::accent()).text_color(theme::on_accent())
                        }
                    })
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .type_micro()
                    .font_weight(theme::SEMIBOLD)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let _ = shell.update(cx, |shell, cx| {
                            if handoff {
                                shell.call_handoff(key.clone(), cx);
                            } else {
                                shell.call_start(key.clone(), cx);
                            }
                        });
                    }))
                    .child(words),
            )
        });
        div()
            .w_full()
            .px(px(self.metrics.inset))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(self.metrics.gap))
            .child(
                div()
                    .w(px(self.metrics.avatar))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .size(px(30.))
                            .rounded_full()
                            .bg(disc_bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph(spec.icon, 14., disc_fg)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .type_small()
                            .font_weight(if spec.missed { theme::SEMIBOLD } else { theme::REGULAR })
                            .text_color(label_fg)
                            .child(spec.label),
                    )
                    .children(faces)
                    .child(div().type_micro().text_color(theme::text_faint()).child(time::clock(message.sent))),
            )
            .children(rate_pill)
            .children(pill)
    }

    fn start_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(open) = &self.open else {
            return div();
        };
        let phone = self.metrics == Metrics::PHONE;
        let size: f32 = if phone { 48. } else { 80. };
        let direct = open.kind.is_direct();
        let words = if direct {
            format!("This is the start of your conversation with {}.", open.title)
        } else {
            format!("This is the start of #{}.", open.title.trim_start_matches('#'))
        };
        // A phone's header already carries the face and the name, so the opening is one quiet
        // line, not a cover that floats in the middle of a short conversation.
        if phone {
            return div()
                .px(px(self.metrics.inset))
                .pt(px(16.))
                .pb(px(12.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(300.))
                        .text_align(gpui::TextAlign::Center)
                        .text_size(px(tokens_phone::scaled(13.)))
                        .line_height(px(tokens_phone::scaled(18.)))
                        .text_color(theme::text_faint())
                        .child(words),
                );
        }
        let plate = if direct { Avatar::new(&open.key, &open.title, size) } else { Avatar::channel("icons/hash.svg", size) };
        let shape = if direct { Shape::Person } else { Shape::Place };
        let (title, file) = (open.title.clone(), open.picture.clone());
        let picture = self.face(plate.shape(shape), file.as_ref(), size, shape, window, cx);
        let gap = if phone { 12. } else { 8. };
        let head: AnyElement = if phone {
            div()
                .text_size(px(tokens_phone::scaled(17.)))
                .line_height(px(tokens_phone::scaled(22.)))
                .font_weight(theme::SEMIBOLD)
                .child(title)
                .into_any_element()
        } else {
            div().type_headline().child(title).into_any_element()
        };
        let line: AnyElement = if phone {
            div()
                .text_size(px(tokens_phone::scaled(13.)))
                .line_height(px(tokens_phone::scaled(18.)))
                .text_color(theme::text_soft())
                .child(words)
                .into_any_element()
        } else {
            div().type_ui().text_color(theme::text_soft()).child(words).into_any_element()
        };
        div()
            .px(px(self.metrics.inset))
            .pt(px(if phone { 16. } else { 32. }))
            .pb(px(12.))
            .flex()
            .flex_col()
            .gap(px(gap))
            .child(picture)
            .child(head)
            .child(line)
    }

    /// An avatar with its picture drawn from this pane's own decodes, asked for at the size it
    /// is shown: the plate holds the box, and the picture covers it once it is here.
    fn face(&mut self, plate: Avatar, file: Option<&MediaFile>, size: f32, shape: Shape, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let edge = media::square_size(size, window.scale_factor());
        let radius = match shape {
            Shape::Person => size / 2.,
            Shape::Place => (size * 0.25).round(),
        };
        let picture = file.and_then(|file| match self.still(file, edge, edge, Some(1.), cx) {
            Picture::Ready(image) => {
                Some(div().absolute().top_0().left_0().size(px(size)).child(anim::picture(image, ObjectFit::Cover, radius, Play::Hover)))
            }
            _ => None,
        });
        plate.build().children(picture)
    }

    fn older_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = div()
            .id("older")
            .h(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .type_small()
            .text_color(theme::text_faint());
        match &self.older_failed {
            None if self.window.messages.is_empty() => row.child("Loading messages…"),
            None => row.child("Loading earlier messages…"),
            Some(_) => row.child(if self.window.messages.is_empty() { "Couldn't load messages ·" } else { "Couldn't load earlier messages ·" }).child(
                div()
                    .id("older-retry")
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .child("Retry")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.older_failed = None;
                        cx.notify();
                    })),
            ),
        }
    }

    /// A failed newer page, jump, or jump-to-latest. Sits above the jump button when that is up.
    pub(super) fn history_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let miss = self.history_miss.as_ref()?;
        let (page, purpose, words) = (miss.page.clone(), miss.purpose.clone(), miss.words.clone());
        let bottom = if self.away { 48. } else { 12. };
        Some(
            div()
                .id("history-miss")
                .absolute()
                .bottom(px(bottom))
                .left(px(16.))
                .max_w(px(420.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::overlay())
                .border_1()
                .border_color(theme::border())
                .shadow(theme::shadow_popover())
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .min_w_0()
                        .type_small()
                        .text_color(theme::text())
                        .child(words),
                )
                .child(
                    div()
                        .id("history-retry")
                        .flex_none()
                        .type_small()
                        .text_color(theme::accent_text())
                        .cursor_pointer()
                        .child("Retry")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.history_miss = None;
                            this.load(page.clone(), purpose.clone(), cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// Floats 16 from the right, 12 above the composer: "12 new" while unread is below, else
    /// "Jump to latest". A click, or Escape with nothing else open, goes to the newest.
    pub(super) fn jump_button(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.multi_select.active {
            return div().into_any_element();
        }
        let words = if self.unseen > 0 { format!("{} new", self.unseen) } else { "Jump to latest".to_owned() };
        if self.metrics == Metrics::PHONE {
            // A phone: a round 44 pt button over the right edge of the list, the arrow alone;
            // with unread below it grows into a pill that says how many, in the accent.
            let unseen = self.unseen > 0;
            return div()
                .id("jump")
                .absolute()
                .bottom(px(16.))
                .right(px(16.))
                .h(px(44.))
                .min_w(px(44.))
                .when(unseen, |button| button.pl(px(12.)).pr(px(16.)))
                .rounded_full()
                .bg(if unseen { theme::accent() } else { theme::overlay() })
                .border_1()
                .border_color(if unseen { theme::accent() } else { theme::border() })
                .shadow(theme::shadow_popover())
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .cursor_pointer()
                .active(|style| style.opacity(0.85))
                .child(glyph("icons/arrow-down.svg", 18., if unseen { theme::on_accent() } else { theme::text() }))
                .children(unseen.then(|| {
                    div()
                        .text_size(px(tokens_phone::scaled(15.)))
                        .line_height(px(tokens_phone::scaled(20.)))
                        .font_weight(theme::SEMIBOLD)
                        .text_color(theme::on_accent())
                        .child(words.clone())
                }))
                .on_click(cx.listener(|this, _, _, cx| this.jump(cx)))
                .into_any_element();
        }
        let unseen = self.unseen > 0;
        div()
            .id("jump")
            .absolute()
            .bottom(px(12.))
            .right(px(16.))
            .h(px(30.))
            .pl(px(10.))
            .pr(px(12.))
            .rounded_full()
            .bg(if unseen { theme::accent() } else { theme::overlay() })
            .border_1()
            .border_color(if unseen { theme::accent() } else { theme::border() })
            .shadow(theme::shadow_popover())
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(move |style| {
                if unseen {
                    style.bg(theme::accent_hover())
                } else {
                    style.bg(theme::over(theme::hover(), theme::overlay()))
                }
            })
            .active(move |style| {
                if unseen {
                    style.bg(theme::accent_pressed())
                } else {
                    style.bg(theme::over(theme::pressed(), theme::overlay()))
                }
            })
            .child(glyph("icons/arrow-down.svg", 15., if unseen { theme::on_accent() } else { theme::text_soft() }))
            .child(
                div()
                    .type_small()
                    .font_weight(if unseen { theme::SEMIBOLD } else { theme::MEDIUM })
                    .text_color(if unseen { theme::on_accent() } else { theme::text() })
                    .child(words),
            )
            .on_click(cx.listener(|this, _, _, cx| this.jump(cx)))
            .into_any_element()
    }

    // The message.

    fn message_row(
        &mut self,
        message: &Rc<Message>,
        head: bool,
        root: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metrics = self.metrics;
        let failed = matches!(message.state, SendState::Failed { .. });
        let pending = message.state == SendState::Pending;
        let mentioned = message.mentions_me && !failed;
        let bar = match (failed, message.mentions_me) {
            (true, _) => Some(theme::danger()),
            (false, true) => Some(theme::accent()),
            (false, false) => None,
        };
        let flashing = self.flash.as_deref() == Some(message.id.as_str());
        // The message in the composer for editing is marked, so it is plain which one is changing.
        let editing = self.shell.upgrade().and_then(|shell| shell.read(cx).composer_editing(cx));
        let editing = !self.is_thread() && editing.as_deref() == Some(message.id.as_str());
        let selecting = self.multi_select.active;
        let ticked = selecting && self.multi_select.contains(&message.id);
        let bar = if editing || ticked {
            Some(theme::accent())
        } else if message.importance != protocol::Importance::Normal {
            Some(theme::danger())
        } else {
            bar
        };
        let owns = self.owns_overlay(&message.id);
        let row_hovered = self.hover.as_deref() == Some(message.id.as_str());
        let phone = metrics == Metrics::PHONE;

        let gutter: Option<AnyElement> = Some(if head {
            let face = self.avatar(message, window, cx);
            self.author_tap(face, "author-face", message, cx)
        } else {
            let ts_tip = message_timestamp_tooltip(message, now());
            div()
                .id(name_id("gts", &message.id))
                .w(px(metrics.avatar))
                .h(if phone { px(tokens_phone::scaled(20.)) } else { theme::line(20.) })
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .type_micro()
                .text_color(if row_hovered { theme::text_faint() } else { gpui::rgba(0) })
                .group_hover("row", |style| style.text_color(theme::text_faint()))
                .when(!phone, |g| g.tooltip(tip(ts_tip, None)))
                .child(short_clock(message.sent))
                .into_any_element()
        });

        let blocked_hidden = !message.outgoing
            && !message.deleted
            && !message.sender.id.is_empty()
            && !self.revealed_blocked.contains(&message.id)
            && self.open.as_ref().is_some_and(|open| !open.kind.is_direct())
            && self
                .shell
                .upgrade()
                .zip(self.open.as_ref())
                .is_some_and(|(shell, open)| {
                    let key = Key::new(&open.key.account(), &message.sender.id);
                    shell
                        .read(cx)
                        .people_state
                        .hub()
                        .person(&key)
                        .is_some_and(|p| p.relation == protocol::Relation::Blocked)
                });
        let sensitive_hidden = !message.deleted
            && !blocked_hidden
            && crate::gui::streamer::is_active()
            && !self.revealed_sensitive.contains(&message.id)
            && crate::gui::streamer::contains_sensitive(&message.text);
        let mut sent = div().flex().flex_col();
        if message.deleted {
            // What it said is gone; the row stays for the thread that hangs off it.
            let tombstone = if message.moderated { "Removed by a moderator" } else { "Message deleted" };
            sent = sent.child(div().max_w(px(MEASURE)).type_body().italic().text_color(theme::text_faint()).child(tombstone));
        } else if blocked_hidden {
            let reveal_id = message.id.clone();
            sent = sent.child(
                div()
                    .max_w(px(MEASURE))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .type_body()
                    .italic()
                    .text_color(theme::text_faint())
                    .child("Blocked message.")
                    .child(
                        div()
                            .id(SharedString::from(format!("reveal-blocked-{}", message.id)))
                            .not_italic()
                            .cursor_pointer()
                            .text_color(theme::text_soft())
                            .hover(|style| style.underline())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.revealed_blocked.insert(reveal_id.clone());
                                cx.notify();
                            }))
                            .child("Show"),
                    ),
            );
        } else if sensitive_hidden {
            let reveal_id = message.id.clone();
            sent = sent.child(sensitive_cover(
                SharedString::from(format!("reveal-sensitive-{}", message.id)),
                cx.listener(move |this, _, _, cx| {
                    this.revealed_sensitive.insert(reveal_id.clone());
                    cx.notify();
                }),
            ));
        } else {
            let said = match &message.kind {
                MessageKind::Unsupported { link } => vec![self.unsupported(message, link, cx).into_any_element()],
                _ => self.content(message, window, cx),
            };
            let attached = self.attachments(message, window, cx);
            sent = match &message.forwarded {
                // When the forwarder wrote a note of their own, render it above the quoted original.
                Some(from) if !from.text.is_empty() && !message.text.is_empty() && from.text != message.text => {
                    let forward_key = format!("forward:{}", message.id);
                    let hide_from = crate::gui::streamer::is_active()
                        && crate::gui::streamer::contains_sensitive(&from.text)
                        && !self.revealed_sensitive.contains(&forward_key);
                    let mut quoted = div().pl(px(12.)).min_w_0().flex_1().flex().flex_col();
                    if !from.author.is_empty() {
                        quoted = quoted.child(
                            div().type_small().font_weight(theme::MEDIUM).text_color(theme::text_soft()).child(from.author.clone()),
                        );
                    }
                    quoted = if hide_from {
                        let key = forward_key;
                        quoted.child(sensitive_cover(
                            SharedString::from(format!("reveal-forward-{}", message.id)),
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.revealed_sensitive.insert(key.clone());
                                cx.notify();
                            }),
                        ))
                    } else {
                        quoted.child(div().max_w(px(MEASURE)).type_body().text_color(theme::text()).child(from.text.clone()))
                    };
                    quoted = quoted.children(attached);
                    sent.children(said)
                        .child(div().mt(px(4.)).child(self.forwarded_line(&message.id, from, cx)))
                        .child(
                            div()
                                .flex()
                                .child(div().w(px(2.)).flex_none().rounded(px(1.)).bg(tokens::border_strong()))
                                .child(quoted),
                        )
                }
                // What was forwarded is the message itself, set behind a bar under where it came from.
                Some(from) => sent.child(self.forwarded_line(&message.id, from, cx)).child(
                    div()
                        .flex()
                        .child(div().w(px(2.)).flex_none().rounded(px(1.)).bg(tokens::border_strong()))
                        .child(div().pl(px(12.)).min_w_0().flex_1().flex().flex_col().children(said).children(attached)),
                ),
                None => sent.children(said).children(attached),
            };
            if self.translating.contains(&message.id) {
                sent = sent.child(
                    div()
                        .mt(px(4.))
                        .pl(px(10.))
                        .border_l_2()
                        .border_color(tokens::border_strong())
                        .type_small()
                        .italic()
                        .text_color(theme::text_faint())
                        .child("Translating…"),
                );
            } else if let Some(translated) = self.translations.get(&message.id).cloned() {
                let clear_id = message.id.clone();
                let attribution = translation_attribution(&translated);
                sent = sent.child(
                    div()
                        .mt(px(4.))
                        .pl(px(10.))
                        .border_l_2()
                        .border_color(theme::accent())
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().max_w(px(MEASURE)).type_body().text_color(theme::text()).child(translated.text))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .type_micro()
                                .text_color(theme::text_faint())
                                .child(attribution)
                                .child("·")
                                .child(
                                    div()
                                        .id(SharedString::from(format!("translation-clear-{}", message.id)))
                                        .role(gpui::Role::Button)
                                        .aria_label("Show original")
                                        .tab_index(0)
                                        .focus_visible(|style| style.shadow(theme::focus_ring()))
                                        .cursor_pointer()
                                        .text_color(theme::text_soft())
                                        .hover(|style| style.underline())
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.translations.remove(&clear_id);
                                            cx.notify();
                                        }))
                                        .child("Show original"),
                                ),
                        ),
                );
            }
            sent = sent
                .children(message.announcement.then(|| {
                    div()
                        .pb(px(3.))
                        .flex()
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(tokens::card())
                                .border_1()
                                .border_color(theme::accent())
                                .type_micro()
                                .font_weight(theme::SEMIBOLD)
                                .text_color(theme::accent_text())
                                .child("ANNOUNCEMENT"),
                        )
                }))
                .children((message.importance != protocol::Importance::Normal).then(|| {
                    let (label, color) = match message.importance {
                        protocol::Importance::Urgent => ("URGENT", theme::danger()),
                        protocol::Importance::Important => ("IMPORTANT", theme::danger()),
                        protocol::Importance::Normal => ("", theme::text_faint()),
                    };
                    div()
                        .pb(px(3.))
                        .flex()
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(tokens::card())
                                .border_1()
                                .border_color(color)
                                .type_micro()
                                .font_weight(theme::SEMIBOLD)
                                .text_color(color)
                                .child(label),
                        )
                }))
                .children(message.effect.as_ref().map(|effect| {
                    div()
                        .pb(px(3.))
                        .flex()
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(tokens::card())
                                .type_micro()
                                .text_color(theme::accent_text())
                                .child(if effect.is_empty() { "✨".to_owned() } else { format!("✨ {effect}") }),
                        )
                }))
                .children(message.sent_via.as_deref().map(|via| {
                    div()
                        .pt(px(2.))
                        .flex()
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(tokens::card())
                                .type_micro()
                                .text_color(theme::text_faint())
                                .child(via.to_owned()),
                        )
                }))
                .children(message.all_cards().enumerate().map(|(ix, card)| self.app_card(message, ix, card, window, cx)))
                .children(message.poll.as_deref().filter(|_| !matches!(message.kind, MessageKind::Unsupported { .. }))
                    .map(|poll| self.poll_card(message, poll, cx)))
                .children(message.contacts.iter().enumerate().map(|(idx, contact)| self.contact_card(message, contact, idx, cx)))
                .children(message.invites.iter().enumerate().map(|(ix, invite)| self.invite_card(message, ix, invite, window, cx)))
                .children(self.message_controls(message, cx))
                .children(self.message_buttons(message, cx))
                .children(self.cards(message, window, cx));
        }
        if pending {
            sent = sent.opacity(0.55);
        }
        // Mine reads as everyone's does, as on the desktop: left, face in the gutter, "You".
        // My newest message's mark goes after its time, or at the row's right edge on its first
        // line when it is grouped under an earlier one and has no header.
        let receipt = self.receipt_on(message);
        let (in_header, at_edge) = if head { (receipt, None) } else { (None, receipt) };
        let column = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .children(head.then(|| self.header(message, in_header, cx)))
            .child(sent)
            .children((!sensitive_hidden).then(|| self.otp_chip(message, cx)).flatten())
            .children(self.publish_chip(message, cx))
            .children(self.views_line(message))
            .children(self.reactions(message, window, cx))
            .children(self.thread_line(message, window, cx))
            .children(self.sending(message, cx))
            .children(self.unsent(message, cx))
            .children(self.ephemeral_note(message, cx));

        let menu = message.clone();
        let keyboard_menu = message.clone();
        let can = self.can(message);
        let inner = div()
            .id(name_id("m", &message.id))
            .role(gpui::Role::ListItem)
            .aria_label(message_label(message, blocked_hidden || sensitive_hidden))
            .tab_index(0)
            .focus_visible(|style| style.shadow(theme::focus_ring_inset()))
            .when(can.act || can.unsent, |row| {
                let pane = cx.weak_entity();
                let message = message.clone();
                row.aria_description("Press Enter to open message actions")
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, cx| {
                        let _ = pane.update(cx, |this, cx| {
                            this.open_menu(message.clone(), this.list.viewport_bounds().center(), window, cx);
                        });
                    })
            })
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.is_keyboard() {
                    let can = this.can(&keyboard_menu);
                    if can.act || can.unsent {
                        this.open_menu(keyboard_menu.clone(), event.position(), window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .group("row")
            .relative()
            .px(px(metrics.inset))
            .when(head, |row| row.pt(px(3.)).pb(px(3.)))
            // A phone gives messages of one group comfortable air; the desktop keeps a clean
            // paragraph flow without clipping ascenders/descenders on hover.
            .when(!head, |row| row.py(px(if metrics == Metrics::PHONE { 5. } else { 2. })))
            .when(editing, |row| row.bg(theme::accent_soft()))
            .when(failed && !editing && !owns, |row| row.bg(theme::danger_soft()))
            .when(mentioned && !editing && !failed, |row| row.bg(theme::accent_wash()))
            // The row that owns an open menu, picker or dialog keeps `selected` (polish/01 §2).
            .when(owns && !editing, |row| row.bg(theme::selected()))
            .when(!mentioned && !editing && !owns && !failed && row_hovered, |row| row.bg(tokens::row_hover()))
            .when(!mentioned && !editing && !owns && !failed, |row| row.hover(|style| style.bg(tokens::row_hover())))
            .when(message.ephemeral && !editing, |row| row.bg(theme::warning_soft()))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    let can = this.can(&menu);
                    if can.act || can.unsent {
                        this.open_menu(menu.clone(), event.position, window, cx);
                    }
                }),
            )
            .children(bar.map(|color| div().absolute().top_0().bottom_0().w(px(2.)).left_0().bg(color)))
            .children(flashing.then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .bg(theme::accent_soft())
                    .with_animation(name_id("flash", &message.id), Animation::new(FLASH), |tint, done| tint.opacity(1. - done))
            }))
            .children(message.via.as_ref().map(|via| command_used(via, metrics, selecting)))
            .children(message.reply_to.as_ref().map(|reply| self.reply_line(reply, &message.id, window, cx)))
            .child(
                div()
                    .flex()
                    .gap(px(metrics.gap))
                    .when(selecting, |row| {
                        let sel_id = message.id.clone();
                        row.child(
                            div()
                                .id(name_id("sel-tick", &message.id))
                                .w(px(18.))
                                .h(px(18.))
                                .mt(px(2.))
                                .flex_none()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(if ticked { theme::accent() } else { tokens::border_strong() })
                                .when(ticked, |box_| box_.bg(theme::accent()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .when(ticked, |box_| box_.child(glyph("icons/check.svg", 12., gpui::rgb(0xffffff))))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                    this.toggle_selected_row(&sel_id, event.modifiers().shift, cx);
                                })),
                        )
                    })
                    .children(gutter)
                    .child(column),
            )
            .when(selecting, |row| {
                let sel_id = message.id.clone();
                row.cursor_pointer().on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    this.toggle_selected_row(&sel_id, event.modifiers().shift, cx);
                }))
            })
            .children(at_edge.map(|receipt| {
                div().absolute().top(px(1.)).right(px(metrics.inset)).h(px(20.)).flex().items_center().child(receipt_mark(&message.id, receipt))
            }))
            // On a phone the actions come up on a long press that holds still (`press_message`),
            // not on a hover a finger never makes. A press that moves past the slop is a scroll.
            .when(metrics == Metrics::PHONE, |row| {
                let down = message.id.clone();
                // A finger's long press comes as its own event: its mouse-down only comes when it
                // lifts, so the press timer never starts for one.
                let held = message.id.clone();
                row.child(crate::gui::phone::long_press(cx.listener(move |this, event: &gpui::LongPressEvent, window, cx| {
                    this.release_message();
                    this.hold_message(&held, event.start_position, window, cx);
                })))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| this.press_message(&down, event.position, window, cx)),
                )
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &gpui::MouseUpEvent, _, _| this.release_message()))
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, _| this.drag_message(event.position)))
            })
            // Last, so it is over the row; it rises past the row's top edge, which nothing clips.
            .children(self.toolbar(message, cx));
        // The gap above a group is outside the row, so it does not light up on hover.
        div().w_full().when(head && !root, |row| row.pt(px(8.))).child(inner).into_any_element()
    }

    /// Discord sender names open the same profile as @mentions. On phones, other networks
    /// also let a person open someone else's profile from their name or face.
    fn author_tap(&self, part: Div, what: &'static str, message: &Message, cx: &mut Context<Self>) -> AnyElement {
        let discord_name = what == "author-name" && self.key().is_some_and(|key| key.network == protocol::Network::Discord);
        let person = self.person(&message.sender.id).filter(|_| discord_name || (self.metrics == Metrics::PHONE && !message.outgoing && !message.sender.bot));
        let Some(person) = person else {
            return part.into_any_element();
        };
        part.id(name_id(what, &message.id))
            .cursor_pointer()
            .when(what == "author-name", |name| name.hover(|style| style.underline()))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                let (person, at) = (person.clone(), window.mouse_position());
                let _ = this.shell.update(cx, |shell, cx| shell.open_profile(person, at, cx));
            }))
            .into_any_element()
    }

    pub(super) fn person(&self, id: &str) -> Option<Key> {
        let open = self.open.as_ref()?;
        (!id.is_empty()).then(|| Key { id: id.to_owned(), ..open.key.clone() })
    }

    /// Someone in this conversation, from what its messages say of them, else from the people
    /// hub: their name and their picture.
    pub(super) fn who(&self, person: &Key, cx: &Context<Self>) -> (String, Option<MediaFile>) {
        if let Some(message) = said_by(self.root(), &self.window.messages, &person.id) {
            return (message.sender.name.clone(), message.sender.avatar.clone());
        }
        let known = self.shell.upgrade().and_then(|shell| shell.read(cx).people_state.hub().person(person));
        known.map(|person| (person.name, person.avatar)).unwrap_or_default()
    }

    fn root(&self) -> Option<&Rc<Message>> {
        self.open.as_ref().and_then(|open| open.root.as_ref()).map(|(root, _)| root)
    }

    /// A Tiny member's picture, already fetched for the server page. The path is not logged.
    fn tiny_sender_picture(&self, user_id: &str, cx: &Context<Self>) -> Option<std::path::PathBuf> {
        let open = self.open.as_ref()?;
        if open.key.network != protocol::Network::Tiny || user_id.is_empty() {
            return None;
        }
        self.shell.upgrade()?.read(cx).tiny_member_picture(&open.key.account(), user_id)
    }

    /// My own picture on this account.
    fn own_picture(&self, cx: &mut Context<Self>) -> Option<MediaFile> {
        let key = self.key()?;
        self.shell.upgrade().and_then(|shell| shell.read(cx).own_picture(key))
    }

    fn avatar(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let sender = &message.sender;
        let size = self.metrics.avatar;
        let me = self.open.as_ref().map(|open| open.me.clone()).unwrap_or_default();
        let id = if message.outgoing && sender.id.is_empty() { me.as_str() } else { sender.id.as_str() };
        let plate = match self.person(id) {
            Some(key) => Avatar::new(&key, &sender.name, size),
            None => Avatar::named(&sender.name, size),
        };
        let file = match (&sender.avatar, message.outgoing) {
            (_, true) => own_face(message, self.root(), &self.window.messages, &me, || self.own_picture(cx)),
            (Some(file), false) => Some(file.clone()),
            // A tombstone names its sender and nothing else; their other messages have the picture.
            (None, false) => self.person(id).and_then(|key| self.who(&key, cx).1),
        };
        // An app never reads as a person: its picture is squared.
        let shape = if sender.bot { Shape::Place } else { Shape::Person };
        // A Tiny picture already on disk for the server page, when the message named none.
        let landed = file.as_ref().and_then(|file| file.path.as_deref()).is_some_and(|path| !path.is_empty());
        if !landed {
            if let Some(path) = self.tiny_sender_picture(id, cx) {
                let shown = plate.shape(shape).image_at(Some(path)).build();
                return div().pt(px(2.)).flex_none().when(message.deleted, |avatar| avatar.opacity(0.4)).child(shown);
            }
        }
        let face = self.face(plate.shape(shape), file.as_ref(), size, shape, window, cx);
        div().pt(px(2.)).flex_none().when(message.deleted, |avatar| avatar.opacity(0.4)).child(face)
    }

    /// The mark this message carries: only my newest, once the network has it, and only while
    /// it is the newest here, as the conversation's row says of it. A thread's replies carry none.
    fn receipt_on(&self, message: &Message) -> Option<Receipt> {
        let receipt = self.open.as_ref()?.receipt.filter(|_| !self.is_thread());
        receipt_for(message, &self.window, receipt)
    }

    /// The chat-level disappearing-messages timer (in seconds), if active for this conversation.
    fn chat_disappearing(&self, cx: &Context<Self>) -> Option<u32> {
        let key = self.key()?;
        self.shell
            .upgrade()?
            .read(cx)
            .held_rows()
            .find(|row| row.key == *key)
            .and_then(|row| row.disappearing)
    }

    /// The head line: who, when, and the marks a message carries.
    fn header(&self, message: &Message, receipt: Option<Receipt>, cx: &mut Context<Self>) -> Div {
        let name = message.shown_sender().to_owned();
        let phone = self.metrics == Metrics::PHONE;
        let current = now();
        let ts_tip = message_timestamp_tooltip(message, current);
        let inline_expiry = message
            .expires_at
            .filter(|&exp| should_show_inline_expiry(exp, current, self.chat_disappearing(cx)))
            .and_then(|exp| expiry_label(exp, current).map(|label| (exp, label)));
        let tag = |words: &'static str| {
            div()
                .flex_none()
                .h(px(16.))
                .px(px(4.))
                .rounded(px(theme::R_CHIP))
                .bg(theme::raised())
                .flex()
                .items_center()
                .text_size(theme::sized(11.))
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::text_soft())
                .child(words)
        };
        div()
            .h(if phone { px(tokens_phone::scaled(20.)) } else { theme::line(20.) })
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(self.author_tap(
                        div()
                            .relative()
                            .min_w_0()
                            .truncate()
                            .type_body()
                            .when(phone, |name| name.text_size(px(tokens_phone::scaled(15.))).line_height(px(tokens_phone::scaled(20.))))
                            .font_weight(theme::SEMIBOLD)
                            .when_some(message.sender.role_color, |name, color| name.text_color(theme::role_color(color)))
                            .child(name),
                        "author-name",
                        message,
                        cx,
                    ))
                    .children(message.sender.role_icon.as_ref().and_then(|f| f.path.as_deref()).filter(|p| !p.is_empty()).map(|path| {
                        gpui::img(std::path::PathBuf::from(path))
                            .size(px(14.))
                            .rounded(px(3.))
                            .flex_none()
                    }))
                    .children(message.sender.role_icon.is_none().then(|| {
                        message.sender.role_emoji.as_deref().filter(|e| !e.is_empty()).map(|emoji| {
                            div().flex_none().text_size(px(13.)).line_height(px(14.)).child(emoji.to_owned())
                        })
                    }).flatten())
                    .child(
                        div()
                            .id(name_id("ts", &message.id))
                            .flex_none()
                            .text_size(if phone { px(tokens_phone::scaled(12.)) } else { theme::sized(11.) })
                            .line_height(if phone { px(tokens_phone::scaled(20.)) } else { theme::line(20.) })
                            .font_weight(theme::MEDIUM)
                            .text_color(theme::text_faint())
                            .when(!phone, |ts| ts.tooltip(tip(ts_tip, None)))
                            .child(time::clock(message.sent)),
                    )
                    .children(receipt.map(|receipt| receipt_mark(&message.id, receipt)))
                    .children(silent_mark(message))
                    .children(inline_expiry.map(|(exp, label)| {
                        div()
                            .id(name_id("expires", &message.id))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .type_micro()
                            .text_color(theme::text_faint())
                            .when(!phone, |el| el.tooltip(tip(expiry_tooltip(exp, current), None)))
                            .child(glyph("icons/clock.svg", 11., theme::text_faint()))
                            .child(label)
                    })),
            )
            .children(message.sender.bot.then(|| tag("APP")))
            .children(message.saved_history.then(|| tag("SAVED")))
            .children(message.published.then(|| tag("PUBLISHED")))
            .children((!message.crosspost_source.is_empty()).then(|| {
                div()
                    .flex_none()
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(format!("from {}", message.crosspost_source))
            }))
            .children(message.pinned.then(|| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(icon(Icon::Pin, 12., theme::text_faint()))
                    .child("Pinned")
            }))
            .children(message.starred.then(|| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(glyph("icons/star.svg", 12., theme::text_faint()))
                    .child("Starred")
            }))
    }

    // Words.

    fn content(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let phone = self.metrics == Metrics::PHONE;
        // An encrypted message this device can't open says so, quietly, and never as its text.
        if message.seal == protocol::Seal::Undecryptable {
            return vec![div()
                .max_w(px(MEASURE))
                .flex()
                .items_center()
                .gap(px(6.))
                .type_body()
                .when(phone, |div| div.text_size(px(tokens_phone::scaled(17.))).line_height(px(tokens_phone::scaled(24.))))
                .italic()
                .text_color(theme::text_faint())
                .child(crate::gui::components::glyph("icons/lock.svg", 12., theme::text_faint()))
                .child(protocol::UNDECRYPTABLE)
                .into_any_element()];
        }
        if message.text.trim().is_empty() {
            return message.edited.then(|| self.edited_mark(message, cx)).into_iter().collect();
        }
        if let Some(emoji) = rich::jumbo(&message.text, &message.spans) {
            let mut out = vec![self.jumbo(&emoji, window, cx)];
            if message.edited {
                out.push(self.edited_mark(message, cx));
            }
            return out;
        }
        let mut blocks = rich::blocks(&message.text, &message.spans);
        let mut edited_alone = message.edited;
        if let (true, Some(Block::Text(last))) = (message.edited, blocks.last_mut()) {
            last.mark_edited();
            edited_alone = false;
        }
        let mut out = Vec::with_capacity(blocks.len() + 1);
        for (ix, block) in blocks.iter().enumerate() {
            let element = match block {
                // A flex column, so the text is a flex item that is measured on its own: in a
                // block its layout belongs to the line box, and `Rich` reads its own layout.
                Block::Text(inline) => {
                    let hovered_run = self
                        .hovered
                        .as_ref()
                        .filter(|(id, at, _)| *id == message.id && *at == ix)
                        .and_then(|(_, _, run)| inline.runs.get(*run));
                    let hovered_time = hovered_run
                        .and_then(|run| run.time)
                        .map(|(unix, _)| time::tooltip(unix));
                    let hovered_edited = hovered_run.is_some_and(|run| run.mark);
                    let block = div()
                        .max_w(px(MEASURE))
                        .flex()
                        .flex_col()
                        .type_body()
                        .when(phone, |div| div.text_size(px(tokens_phone::scaled(17.))).line_height(px(tokens_phone::scaled(24.))))
                        .child(self.rich(message, ix, inline, theme::text().into_color(), window, cx));
                    if let Some(words) = hovered_time {
                        block.id(name_id(&format!("time-{ix}"), &message.id)).tooltip(tip(words, None)).into_any_element()
                    } else if hovered_edited {
                        block.id(name_id("edited", &message.id)).tooltip(tip(edited_tip(message.edited_at), None)).into_any_element()
                    } else {
                        block.into_any_element()
                    }
                }
                Block::Heading { level, text } => {
                    let (size, line, top, bottom) = match *level {
                        1 => (20., 26., 6., 2.),
                        2 => (18., 24., 4., 1.),
                        _ => (16., 22., 2., 0.),
                    };
                    div()
                        .max_w(px(MEASURE))
                        .mt(px(top))
                        .mb(px(bottom))
                        .flex()
                        .flex_col()
                        .text_size(theme::sized(size))
                        .line_height(theme::sized(line))
                        .font_weight(theme::SEMIBOLD)
                        .when(phone, |div| {
                            div.text_size(px(tokens_phone::scaled(size + 2.)))
                                .line_height(px(tokens_phone::scaled(line + 2.)))
                        })
                        .child(self.rich(message, ix, text, theme::text().into_color(), window, cx))
                        .into_any_element()
                }
                Block::Small(inline) => div()
                    .max_w(px(MEASURE))
                    .mt(px(1.))
                    .flex()
                    .flex_col()
                    .type_small()
                    .when(phone, |div| div.text_size(px(tokens_phone::scaled(13.))).line_height(px(tokens_phone::scaled(18.))))
                    .text_color(theme::text_faint())
                    .child(self.rich(message, ix, inline, theme::text_faint().into_color(), window, cx))
                    .into_any_element(),
                Block::Quote(inline) => {
                    let hovered_time = self
                        .hovered
                        .as_ref()
                        .filter(|(id, at, _)| *id == message.id && *at == ix)
                        .and_then(|(_, _, run)| inline.runs.get(*run))
                        .and_then(|run| run.time)
                        .map(|(unix, _)| time::tooltip(unix));
                    let block = div()
                        .max_w(px(MEASURE))
                        .mt(px(3.))
                        .flex()
                        .child(div().w(px(3.)).flex_none().rounded(px(2.)).bg(tokens::border_strong()))
                        .child(
                            div()
                                .pl(px(12.))
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .type_body()
                                .when(phone, |div| div.text_size(px(tokens_phone::scaled(17.))).line_height(px(tokens_phone::scaled(24.))))
                                .child(self.rich(message, ix, inline, theme::text_soft().into_color(), window, cx)),
                        );
                    if let Some(words) = hovered_time {
                        block.id(name_id(&format!("time-{ix}"), &message.id)).tooltip(tip(words, None)).into_any_element()
                    } else {
                        block.into_any_element()
                    }
                }
                Block::Item { ordered, depth, number, text } => {
                    let marker = if *ordered { format!("{number}.") } else { "•".to_owned() };
                    let hovered_time = self
                        .hovered
                        .as_ref()
                        .filter(|(id, at, _)| *id == message.id && *at == ix)
                        .and_then(|(_, _, run)| text.runs.get(*run))
                        .and_then(|run| run.time)
                        .map(|(unix, _)| time::tooltip(unix));
                    let block = div()
                        .max_w(px(MEASURE))
                        .pl(px(22. * f32::from(*depth)))
                        .flex()
                        .type_body()
                        .when(phone, |div| div.text_size(px(tokens_phone::scaled(17.))).line_height(px(tokens_phone::scaled(24.))))
                        .child(div().w(px(22.)).flex_none().text_color(theme::text_faint()).child(marker))
                        .child(div().min_w_0().flex_1().flex().flex_col().child(self.rich(message, ix, text, theme::text().into_color(), window, cx)));
                    if let Some(words) = hovered_time {
                        block.id(name_id(&format!("time-{ix}"), &message.id)).tooltip(tip(words, None)).into_any_element()
                    } else {
                        block.into_any_element()
                    }
                }
                Block::Code { language, text } => self.code_block(message, ix, language, text, cx),
            };
            out.push(element);
        }
        if edited_alone {
            out.push(self.edited_mark(message, cx));
        }
        out
    }

    /// Inline "Copy <code>" chip for an incoming message containing a one-time verification code.
    /// Only copies when clicked; never auto-copies.
    fn otp_chip(&mut self, message: &Message, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.outgoing || message.deleted || message.kind != MessageKind::Normal {
            return None;
        }
        let code = protocol::otp::extract_code(&message.text)?;
        let key = (message.id.clone(), usize::MAX);
        let copied = self.copied.as_ref() == Some(&key);
        let label = if copied {
            "Copied".to_owned()
        } else {
            format!("Copy code {code}")
        };
        let copy = key.clone();
        let chip = div()
            .id(name_id("otp", &message.id))
            .mt(px(4.))
            .h(px(26.))
            .px(px(10.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::field())
            .border_1()
            .border_color(theme::border())
            .hover(|style| style.bg(theme::selected()).border_color(theme::border_strong()))
            .active(|style| style.bg(theme::pressed()))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::text())
            .cursor_pointer()
            .child(icon(if copied { Icon::Check } else { Icon::Copy }, 12., theme::text_soft()))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                this.copied = Some(copy.clone());
                cx.notify();
                let copy = copy.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.copied.as_ref() == Some(&copy) {
                            this.copied = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }));
        Some(div().flex().child(chip).into_any_element())
    }

    /// One-click "Publish" chip under my own unpublished messages in an announcement channel.
    fn publish_chip(&mut self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !message.outgoing || !self.can(message).publish {
            return None;
        }
        let msg = message.clone();
        let chip = div()
            .id(name_id("publish", &message.id))
            .mt(px(4.))
            .h(px(26.))
            .px(px(10.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::field())
            .border_1()
            .border_color(theme::border())
            .hover(|style| style.bg(theme::selected()).border_color(theme::border_strong()))
            .active(|style| style.bg(theme::pressed()))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::text())
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(crate::gui::components::glyph("icons/megaphone.svg", 12., theme::text_soft()))
            .child("Publish")
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.publish_message(&msg, cx);
            }));
        Some(div().flex().child(chip).into_any_element())
    }

    /// Where a forwarded message was first said, with an in-app "→ origin" link when present.
    fn forwarded_line(&self, message_id: &str, from: &Forwarded, cx: &mut Context<Self>) -> Div {
        let place = match (from.from_title.is_empty(), from.from_space.is_empty()) {
            (true, true) => "Forwarded".to_owned(),
            (false, true) => format!("Forwarded from {}", from.from_title),
            (true, false) => format!("Forwarded from {}", from.from_space),
            (false, false) => format!("Forwarded from {} in {}", from.from_title, from.from_space),
        };
        let origin = (!from.link.is_empty()).then(|| {
            let url = from.link.clone();
            div()
                .id(name_id("fwd-origin", message_id))
                .flex_none()
                .cursor_pointer()
                .text_color(tokens::link())
                .hover(|s| s.underline())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_link(&url, event.modifiers().alt, window, cx);
                }))
                .child("→ origin")
        });
        div()
            .h(px(18.))
            .mb(px(2.))
            .flex()
            .items_center()
            .gap(px(5.))
            .type_small()
            .text_color(theme::text_faint())
            .child(icon(Icon::Forward, 13., theme::text_faint()))
            .child(div().min_w_0().truncate().child(place))
            .children(origin)
    }

    /// "(edited)", with the time it was edited. A network that keeps earlier wordings opens them on a click.
    fn edited_mark(&mut self, message: &Message, cx: &mut Context<Self>) -> AnyElement {
        let words = edited_tip(message.edited_at);
        let id = message.id.clone();
        let history = self.keeps_revisions(cx);
        div()
            .id(name_id("edited", &id))
            .h(px(16.))
            .type_micro()
            .text_color(theme::text_faint())
            .tooltip(tip(words, None))
            .when(history, |mark| {
                mark.cursor_pointer().on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    this.open_revisions(&id, event.position(), cx);
                }))
            })
            .child("(edited)")
            .into_any_element()
    }

    /// This account's network keeps earlier wordings (`Capabilities::revisions`).
    fn keeps_revisions(&self, cx: &gpui::App) -> bool {
        let Some(key) = self.key().cloned() else {
            return false;
        };
        self.shell
            .upgrade()
            .and_then(|shell| {
                shell.read(cx).snapshot.as_ref().and_then(|snapshot| snapshot.account(&key.account())).map(|account| account.can.revisions)
            })
            .unwrap_or(false)
    }

    /// Asks for earlier wordings and opens the list at the click.
    fn open_revisions(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.keeps_revisions(cx) {
            return;
        }
        let Some(conversation) = self.key().cloned() else {
            return;
        };
        self.menu = None;
        let message_id = id.to_owned();
        self.revisions = Some(super::Earlier { message: message_id.clone(), at, rows: Vec::new(), ready: false });
        cx.notify();
        let request = Request::Revisions { conversation, message_id: message_id.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(open) = this.revisions.as_mut() else {
                    return;
                };
                if open.message != message_id {
                    return;
                }
                open.ready = true;
                if let Ok(Response::Revisions { revisions, .. }) = answer {
                    open.rows = revisions;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the Teams tag members popover for a clicked `@Tag` mention span.
    fn open_tag_popover(&mut self, space: Key, tag_id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = None;
        self.revisions = None;
        self.tag_popover = Some(super::TagPopover {
            space: space.clone(),
            tag_id: tag_id.clone(),
            label: "Tag".to_owned(),
            at,
            tag: None,
            ready: false,
        });
        cx.notify();
        let request = Request::SpaceTags { space: space.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(pop) = this.tag_popover.as_mut() else {
                    return;
                };
                if pop.space != space || pop.tag_id != tag_id {
                    return;
                }
                pop.ready = true;
                if let Ok(Response::SpaceTags { tags, .. }) = answer {
                    if let Some(found) = tags.into_iter().find(|t| t.id == tag_id) {
                        pop.label = found.name.clone();
                        pop.tag = Some(found);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Renders the Teams `@Tag` members popover with a "Start group chat" action and
    /// "Manage tags in Teams" external link.
    pub(super) fn tag_popover_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pop = self.tag_popover.as_ref()?;
        let heading = format!("@{}", pop.label);
        let mut card = div()
            .id("tag-popover")
            .w(px(260.))
            .p(px(10.))
            .rounded(px(theme::R_FLOAT))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_popover())
            .occlude()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).truncate().child(heading))
                    .child(
                        div()
                            .id("tag-popover-close")
                            .size(px(20.))
                            .rounded(px(theme::R_CHIP))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .child(glyph("icons/x.svg", 14., theme::text_soft()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tag_popover = None;
                                cx.notify();
                            })),
                    ),
            );
        if !pop.ready {
            card = card.child(div().type_small().text_color(theme::text_faint()).child("Loading members…"));
        } else if let Some(tag) = pop.tag.as_ref() {
            if !tag.description.is_empty() {
                card = card.child(div().type_small().text_color(theme::text_soft()).child(tag.description.clone()));
            }
            card = card.child(div().type_small().text_color(theme::text_faint()).child(format!("{} members", tag.member_count)));
            for (mix, member) in tag.members.iter().take(12).enumerate() {
                let line = if member.title.is_empty() {
                    member.name.clone()
                } else {
                    format!("{} · {}", member.name, member.title)
                };
                card = card.child(
                    div()
                        .id(("tag-pop-member", mix))
                        .type_small()
                        .text_color(theme::text())
                        .truncate()
                        .child(line),
                );
            }
            let people: Vec<Key> = tag.members.iter().map(|m| m.key.clone()).collect();
            let mut footer = div().pt(px(4.)).flex().items_center().justify_between().gap(px(6.));
            if !people.is_empty() {
                let account = pop.space.account();
                let title = tag.name.clone();
                footer = footer.child(button("tag-pop-chat", ButtonKind::Primary, "Start group chat").h(px(24.)).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.tag_popover = None;
                        cx.notify();
                        let req = Request::StartGroup {
                            account: account.clone(),
                            people: people.clone(),
                            title: Some(title.clone()),
                        };
                        let shell = this.shell.clone();
                        crate::gui::people::ask_later(cx, req, move |answer, cx| {
                            if let Ok(Response::Conversation { conversation }) = answer {
                                let _ = shell.update(cx, |shell, cx| shell.select(conversation.key, &conversation.title, cx));
                            }
                        });
                    }),
                ));
            }
            footer = footer.child(button("tag-pop-manage", ButtonKind::Ghost, "Manage in Teams ↗").h(px(24.)).on_click(
                cx.listener(|this, _, window, cx| {
                    this.tag_popover = None;
                    this.open_external_link("https://teams.microsoft.com/", window, cx);
                    cx.notify();
                }),
            ));
            card = card.child(footer);
        } else {
            card = card
                .child(div().type_small().text_color(theme::text_faint()).child("Tag details unavailable."))
                .child(
                    button("tag-pop-manage-fallback", ButtonKind::Ghost, "Manage tags in Teams ↗").h(px(24.)).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.tag_popover = None;
                            this.open_external_link("https://teams.microsoft.com/", window, cx);
                            cx.notify();
                        }),
                    ),
                );
        }
        Some(deferred(anchored().position(pop.at).child(card)).into_any_element())
    }

    /// The decoded picture of a file on disk, `edge` pixels on its longer side. Asking queues
    /// the decode; the row is drawn again when it is done.
    pub(super) fn picture(&mut self, path: &str, edge: u32, crop: Option<f32>, cx: &mut Context<Self>) -> Picture {
        self.images.get(path, edge, crop, cx, Self::decoded)
    }

    pub(super) fn decoded(&mut self, loaded: super::images::Loaded, cx: &mut Context<Self>) {
        if self.images.store(loaded) {
            cx.notify();
        }
        self.images.pump(cx, Self::decoded);
    }

    fn rich(
        &mut self,
        message: &Message,
        block: usize,
        inline: &Inline,
        color: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Rich {
        let hovered = self.hovered.as_ref().filter(|(id, at, _)| *id == message.id && *at == block).map(|(_, _, run)| *run);
        let selected = self.selection.as_ref().filter(|s| s.message == message.id);
        let selection = selected.and_then(|s| s.within(block));
        let dragging = self.dragging && selected.is_some();
        let revealed: Vec<u32> = self.revealed.iter().filter(|(id, _)| *id == message.id).map(|(_, span)| *span).collect();
        let me = self.open.as_ref().map(|open| open.me.clone()).unwrap_or_default();
        let edge = media::square_size(18., window.scale_factor());
        let this = cx.weak_entity();
        let id = message.id.clone();
        let history = self.keeps_revisions(cx) && message.edited;
        let mut emoji = |file: &MediaFile| match self.still(file, edge, edge, None, cx) {
            Picture::Ready(image) => Some(image),
            _ => None,
        };
        let look = Look {
            color,
            hovered,
            revealed: &|span| revealed.contains(&span),
            me: &me,
            mentions_me: message.mentions_me,
            emoji: &mut emoji,
            selection,
            dragging,
            history,
        };
        Rich::new(("text", block), inline, look, move |event, window, cx| {
            let _ = this.update(cx, |this, cx| this.text_event(&id, block, event, window, cx));
        })
    }

    fn text_event(&mut self, id: &str, block: usize, event: TextEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TextEvent::Hover(Some(run)) => {
                self.hovered = Some((id.to_owned(), block, run));
                cx.notify();
            }
            TextEvent::Hover(None) => {
                // Only the block that had the pointer may say it has gone.
                if self.hovered.as_ref().is_some_and(|(at, in_block, _)| at == id && *in_block == block) {
                    self.hovered = None;
                    cx.notify();
                }
            }
            TextEvent::Down(at) => {
                // Shift extends what is selected in this message; a plain press starts over.
                let extend = window.modifiers().shift;
                match self.selection.as_mut().filter(|selection| extend && selection.message == id) {
                    Some(selection) => selection.head = Some((block, at)),
                    None => self.selection = Some(Selection { message: id.to_owned(), anchor: (block, at), head: None }),
                }
                self.dragging = true;
                self.on_text = true;
                cx.notify();
            }
            TextEvent::Drag(at) => {
                // Only the message the drag began in: its blocks speak, the others stay quiet.
                let Some(selection) = self.selection.as_mut().filter(|selection| self.dragging && selection.message == id) else {
                    return;
                };
                if selection.head != Some((block, at)) {
                    selection.head = Some((block, at));
                    cx.notify();
                }
            }
            TextEvent::Click(run_ix, alt) => {
                let Some(message) = self.find(id) else {
                    return;
                };
                let mut blocks = rich::blocks(&message.text, &message.spans);
                if let (true, Some(Block::Text(last))) = (message.edited, blocks.last_mut()) {
                    last.mark_edited();
                }
                let inline = match blocks.get(block) {
                    Some(
                        Block::Text(inline)
                        | Block::Heading { text: inline, .. }
                        | Block::Small(inline)
                        | Block::Quote(inline)
                        | Block::Item { text: inline, .. },
                    ) => inline,
                    _ => return,
                };
                let run_text = inline
                    .ranges()
                    .nth(run_ix)
                    .and_then(|(range, _)| inline.text.get(range))
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                let Some(run) = inline.runs.get(run_ix) else {
                    return;
                };
                if run.mark {
                    self.open_revisions(id, window.mouse_position(), cx);
                    return;
                }
                if let Some(spoiler) = run.spoiler {
                    if self.revealed.insert((id.to_owned(), spoiler)) {
                        cx.notify();
                        return;
                    }
                }
                if let Some(url) = &run.link {
                    // A sponsored message's follow is Telegram's to count; the link opens all the same.
                    if message.card.as_deref().is_some_and(|card| card.sponsored) {
                        if let Some(conversation) = self.key().cloned() {
                            let request = Request::FollowLink { conversation, message_id: id.to_owned() };
                            cx.background_executor().spawn(async move { ask(&request) }).detach();
                        }
                    }
                    self.open_link(url, alt, window, cx);
                } else if let Some(action) = &run.action {
                    match action {
                        RunAction::Hashtag if !run_text.is_empty() => {
                            let _ = self.shell.update(cx, |shell, cx| {
                                shell.open_search_with(super::side::Scope::Conversation, &run_text, window, cx);
                            });
                        }
                        RunAction::Command if !run_text.is_empty() => {
                            if let Some(conversation) = self.key().cloned() {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_millis());
                                let request = Request::Send {
                                    conversation,
                                    client_id: format!("cmd-{now}"),
                                    text: run_text,
                                    reply_to: None,
                                    with: protocol::Outgoing::default(),
                                };
                                cx.spawn(async move |this, cx| {
                                    let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
                                    if let Ok(Response::Error { detail, .. }) = answer {
                                        let _ = this.update(cx, |this, cx| this.say(detail, cx));
                                    }
                                })
                                .detach();
                            }
                        }
                        RunAction::Username(username) if !username.is_empty() => {
                            let url = format!("https://t.me/{username}");
                            self.open_link(&url, alt, window, cx);
                        }
                        RunAction::Copyable if !run_text.is_empty() => {
                            cx.write_to_clipboard(ClipboardItem::new_string(run_text.clone()));
                            let _ = self.shell.update(cx, |shell, cx| shell.toast(None, format!("Copied {run_text}"), cx));
                        }
                        _ => {}
                    }
                } else if let Some((MentionKind::User, person)) = &run.mention {
                    let (Some(person), at) = (self.person(person), window.mouse_position()) else {
                        return;
                    };
                    let _ = self.shell.update(cx, |shell, cx| shell.open_profile(person, at, cx));
                } else if let Some((MentionKind::Role, tag_id)) = &run.mention {
                    if let (Some(open), at) = (self.open.as_ref(), window.mouse_position()) {
                        if open.key.network == Network::Teams {
                            let space_id = self.shell.upgrade().and_then(|shell| {
                                let shell = shell.read(cx);
                                shell.snapshot.as_ref()?.open.as_ref()?.space.clone()
                            });
                            if let Some(space_id) = space_id {
                                let space = Key::new(&open.key.account(), space_id);
                                self.open_tag_popover(space, tag_id.clone(), at, cx);
                            }
                        }
                    }
                } else if let Some((MentionKind::Channel, channel_id)) = &run.mention {
                    let channel_id = channel_id.clone();
                    self.open_channel_mention(&channel_id, alt, window, cx);
                } else if matches!(run.mention, Some((MentionKind::Command, _))) {
                    let shown = inline
                        .ranges()
                        .find(|(_, one)| std::ptr::eq(*one, run))
                        .map(|(range, _)| &inline.text[range])
                        .unwrap_or("");
                    let name = shown.trim().trim_start_matches('/').to_owned();
                    if !name.is_empty() {
                        let _ = self.shell.update(cx, |shell, cx| shell.composer_fill_command(&name, window, cx));
                    }
                }
            }
        }
    }

    fn jumbo(&mut self, emoji: &[Jumbo], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let edge = media::square_size(40., window.scale_factor());
        let mut line = div().h(px(48.)).flex().items_center().gap(px(4.));
        for one in emoji {
            line = match one {
                Jumbo::Unicode(glyphs) => line.child(div().text_size(px(40.)).line_height(px(48.)).child(glyphs.clone())),
                Jumbo::Custom { file, name } => {
                    match self.still(file, edge, edge, None, cx) {
                        Picture::Ready(image) => line.child(div().size(px(40.)).flex_none().child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline))),
                        _ => line.child(div().type_body().text_color(theme::text_soft()).child(name.clone())),
                    }
                }
            };
        }
        line.into_any_element()
    }

    /// Colours for this block, started off the UI thread the first time it is drawn.
    fn code_colors(&mut self, message: &Message, block: usize, language: &str, text: &str, cx: &mut Context<Self>) -> Option<Rc<super::highlight::Lit>> {
        let mut hasher = DefaultHasher::new();
        language.hash(&mut hasher);
        text.hash(&mut hasher);
        let stamp = hasher.finish();
        let key = (message.id.clone(), block);
        if let Some((have, lit)) = self.code_colors.get(&key) {
            if *have == stamp {
                return Some(Rc::clone(lit));
            }
        }
        if !self.code_coloring.insert(key.clone()) {
            return self.code_colors.get(&key).map(|(_, lit)| Rc::clone(lit));
        }
        let language = language.to_owned();
        let source = text.to_owned();
        let source_len = source.len();
        let id = message.id.clone();
        cx.spawn(async move |this, cx| {
            let lit = cx.background_executor().spawn(async move { super::highlight::highlight(&language, &source) }).await;
            let _ = this.update(cx, |this, cx| {
                this.code_coloring.remove(&key);
                if this.code_colors.len() >= 64 {
                    this.code_colors.clear();
                }
                let resized = lit.text.len() != source_len;
                this.code_colors.insert(key, (stamp, Rc::new(lit)));
                if resized {
                    for (ix, item) in this.items.iter().enumerate() {
                        if item.message().is_some_and(|message| message.id == id) {
                            this.list.splice(ix..ix + 1, 1);
                            break;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        None
    }

    fn code_block(&mut self, message: &Message, block: usize, language: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let key = (message.id.clone(), block);
        let lines = text.lines().count();
        let folded = lines > CODE_LINES && !self.expanded.contains(&key);
        let lit = if lines > super::highlight::PLAIN_LINES { None } else { self.code_colors(message, block, language, text, cx) };
        let shown = match &lit {
            Some(lit) if folded => first_lines(&lit.text, CODE_LINES).to_owned(),
            Some(lit) => lit.text.clone(),
            None if folded => text.lines().take(CODE_LINES).collect::<Vec<_>>().join("\n"),
            None => text.to_owned(),
        };
        let highlights = code_highlights(&shown, lit.as_ref().map(|lit| lit.runs.as_slice()).unwrap_or(&[]));
        let copied = self.copied.as_ref() == Some(&key);
        let (copy, text) = (key.clone(), text.to_owned());
        // A phone: the block is the column's width and the code scrolls sideways inside it; its
        // copy button is a thumb's 36 and the code follows the phone's run.
        let phone = self.metrics == Metrics::PHONE;
        let mut body = div().id(("code", block)).overflow_x_scroll().px(px(12.)).pt(px(2.)).pb(px(9.));
        // The wheel over a code block still scrolls the conversation; only sideways scrolls the code.
        body.style().restrict_scroll_to_axis = Some(true);
        div()
            .mt(px(5.))
            .mb(px(3.))
            .max_w(px(MEASURE))
            .rounded(px(theme::R_CARD))
            .bg(tokens::code_bg())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .child(
                div()
                    .h(px(if phone { 36. } else { 28. }))
                    .pl(px(12.))
                    .pr(px(2.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_micro().text_color(theme::text_faint()).child(language.to_owned()))
                    .child(
                        tool(("copy", block), if copied { Icon::Check } else { Icon::Copy }, if phone { 36. } else { 24. })
                            .when(!phone, |copy| copy.tooltip(tip("Copy code", None)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                this.copied = Some(copy.clone());
                                cx.notify();
                                let copy = copy.clone();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.copied.as_ref() == Some(&copy) {
                                            this.copied = None;
                                            cx.notify();
                                        }
                                    });
                                })
                                .detach();
                            })),
                    ),
            )
            .child(crate::gui::components::scroll::scroll_content(("code-scroll-frame", block), body.pb(px(10.)).child(
                div()
                    .type_code()
                    .when(phone, |code| code.text_size(px(tokens_phone::scaled(14.))).line_height(px(tokens_phone::scaled(20.))))
                    .whitespace_nowrap()
                    .child(if highlights.is_empty() {
                        shown.into_any_element()
                    } else {
                        StyledText::new(SharedString::from(shown)).with_highlights(highlights).into_any_element()
                    }),
            )))
            .children((lines > CODE_LINES).then(|| {
                let collapsing = !folded;
                div()
                    .id(("more", block))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .type_small()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .child(if collapsing {
                        "Show less".to_owned()
                    } else {
                        format!("Show {} more lines", lines - CODE_LINES)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if collapsing {
                            this.expanded.remove(&key);
                        } else {
                            this.expanded.insert(key.clone());
                        }
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    fn unsupported(&self, message: &Message, link: &str, cx: &mut Context<Self>) -> Div {
        let network = self.open.as_ref().map_or("the app", |open| open.key.network.label());
        let which = if message.text.starts_with("Poll") { Icon::Poll } else { Icon::File };
        let link = link.to_owned();
        let line = div()
            .h(px(36.))
            .max_w_full()
            .px(px(12.))
            .rounded(px(theme::R_CARD))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .type_ui()
            .child(icon(which, 16., theme::text_soft()))
            .child(div().min_w_0().truncate().text_color(theme::text_soft()).child(message.text.clone()))
            .children((!link.is_empty()).then(|| {
                div()
                    .id("open-in")
                    .flex_none()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(format!("Open in {network}"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&link)))
            }));
        // A row of its own, so the box is as wide as its words and no wider.
        div().mt(px(2.)).flex().child(line)
    }

    // Pictures and files.

    fn attachments(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if message.attachments.is_empty() {
            return Vec::new();
        }
        let width = self.media_width();
        let shapes: Vec<MediaShape> = message.attachments.iter().map(media::shape).collect();
        let visual: Vec<usize> = (0..shapes.len()).filter(|ix| shapes[*ix].is_visual()).collect();
        let mut out = Vec::new();
        match visual.as_slice() {
            [] => {}
            [only] => {
                let attachment = &message.attachments[*only];
                let (wide, tall) = natural(attachment);
                let single = media::single(wide, tall, width);
                let tile = self.tile(message, *only, single.width, single.height, single.fit, None, window, cx);
                if attachment.round {
                    let btn = self.transcribe_button(message, *only, cx);
                    let block = self.transcript_block(message, *only, cx);
                    if btn.is_some() || block.is_some() {
                        out.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(div().flex().items_end().gap(px(6.)).child(tile).children(btn))
                                .children(block)
                                .into_any_element(),
                        );
                    } else {
                        out.push(tile);
                    }
                } else if let Some(panel) = self.transcript_box(message, *only, None, cx) {
                    out.push(div().flex().flex_col().child(tile).child(panel).into_any_element());
                } else {
                    out.push(tile);
                }
            }
            many => {
                let (tiles, height) = media::grid(many.len(), width);
                let mut grid = div().relative().w(px(width)).h(px(height));
                for (slot, tile) in tiles.iter().enumerate() {
                    let more = (slot == 3 && many.len() > 4).then(|| many.len() - 4);
                    let picture = self.tile(message, many[slot], tile.width, tile.height, Fit::Cover, more, window, cx);
                    grid = grid.child(div().absolute().left(px(tile.x)).top(px(tile.y)).child(picture));
                }
                out.push(grid.into_any_element());
            }
        }
        for (ix, shape) in shapes.iter().enumerate() {
            match shape {
                MediaShape::Sticker => out.push(self.sticker(message, ix, window, cx)),
                MediaShape::Voice => out.push(self.player_row(message, ix, true, cx)),
                MediaShape::Audio => out.push(self.player_row(message, ix, false, cx)),
                MediaShape::ViewOnce => out.push(self.view_once_card(message, ix)),
                MediaShape::File => out.push(self.file_row(message, ix, cx)),
                MediaShape::Image | MediaShape::Gif | MediaShape::Video => {}
            }
        }
        // 5 above the first, 4 between.
        out.into_iter()
            .enumerate()
            .map(|(ix, element)| div().pt(px(if ix == 0 { 5. } else { 4. })).flex().child(element).into_any_element())
            .collect()
    }

    /// Compact cover card for view-once media so thumbnails are never rendered inline.
    fn view_once_card(&self, message: &Message, index: usize) -> AnyElement {
        let attachment = &message.attachments[index];
        let kind_label = match attachment.file.kind {
            protocol::MediaKind::Image | protocol::MediaKind::Gif => "View-once photo",
            protocol::MediaKind::Video => "View-once video",
            protocol::MediaKind::Audio => "View-once voice message",
            _ => "View-once media",
        };
        div()
            .id(("view-once", index))
            .h(px(40.))
            .px(px(12.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .child(glyph("icons/eye-off.svg", 15., theme::text_soft()))
            .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(kind_label))
            .child(div().type_small().text_color(theme::text_faint()).child("· Open on phone"))
            .into_any_element()
    }

    /// Settings › Privacy › Load images (P3) off: a picture not on disk waits for a click, so
    /// nothing is fetched until then.
    fn held_back(&self, file: &MediaFile) -> bool {
        !crate::gui::app::prefs::choices().images && file.path.is_none() && !self.known.contains_key(&file.id) && !self.shown.contains(&file.id)
    }

    /// What stands in a held-back picture's box: what it is, its size, and the click that
    /// loads it.
    fn load_box(&self, holder: Stateful<Div>, file: &MediaFile, what: &str, bytes: u64, words: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = file.id.clone();
        let tap = if crate::gui::phone::active() { "Tap to load" } else { "Click to load" };
        let line = match (words, bytes) {
            (false, _) => "Load".to_owned(),
            (true, 0) => format!("{what} · {tap}"),
            (true, bytes) => format!("{what} · {} · {tap}", media::size_label(bytes)),
        };
        holder
            .child(div().absolute().inset_0().flex().items_center().justify_center().type_small().text_color(theme::text_soft()).child(line))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.shown.insert(id.clone());
                cx.notify();
            }))
    }

    /// The still of a file, or what stands in its box until it is on disk. `size` is the
    /// longest side the whole picture is shown at, which is what chatd is asked for; `edge` is
    /// what it is decoded to.
    pub(super) fn still(&mut self, file: &MediaFile, size: u32, edge: u32, crop: Option<f32>, cx: &mut Context<Self>) -> Picture {
        self.want(file, size);
        let path = file.path.clone().or_else(|| self.known.get(&file.id).cloned());
        match path {
            // A smaller copy on disk is drawn while a larger one is fetched, and if that fails.
            Some(path) => self.picture(&path, edge, crop, cx),
            None if file.failed.is_some() => Picture::Failed,
            None => Picture::Loading,
        }
    }

    /// The few pixels of a picture that came inside its message (`Attachment::preview`),
    /// decoded at their own size: stretched to the still's box they draw soft. `None` while
    /// they are not on disk, and when they could not be read: the box keeps its placeholder.
    /// A spoiler not yet revealed and a picture Load images holds back never get this far.
    fn preview(&mut self, attachment: &Attachment, crop: Option<f32>, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let preview = attachment.preview.as_ref()?;
        let edge = preview.width.max(preview.height).max(1);
        match self.still(preview, edge, edge, crop, cx) {
            Picture::Ready(image) => Some(image),
            Picture::Loading | Picture::Failed => None,
        }
    }

    /// What a picture's box shows when its file could not be fetched: the reason on hover, and
    /// a Retry that asks once per click.
    fn failed_box(&self, holder: Stateful<Div>, file: &MediaFile, words: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let reason = file.failed.clone().unwrap_or_else(|| "The file could not be read.".to_owned());
        let retry = file.clone();
        holder
            .child(in_box(Icon::ImageOff, words.then_some("Couldn't load image · Retry")))
            .tooltip(tip(format!("{reason} Click to try again."), None))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.retry_file(&retry, cx);
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn tile(
        &mut self,
        message: &Rc<Message>,
        index: usize,
        width: f32,
        height: f32,
        fit: Fit,
        more: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let attachment = &message.attachments[index];
        let shape = media::shape(attachment);
        let still = attachment.thumb.as_ref().unwrap_or(&attachment.file).clone();
        let edge_line = || div().absolute().inset_0().rounded(px(theme::R_CARD)).border_1().border_color(theme::hairline());
        let tile = div()
            .id(("tile", index))
            .relative()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .rounded(px(theme::R_CARD))
            .bg(tokens::media_fill())
            .overflow_hidden()
            .cursor_pointer();

        if media::is_spoiler(attachment) && !self.shown.contains(&attachment.file.id) {
            let id = attachment.file.id.clone();
            return tile
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .type_small()
                        .text_color(theme::text_faint())
                        .child(if width >= 160. { "Spoiler · click to show" } else { "Spoiler" }),
                )
                .child(edge_line())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shown.insert(id.clone());
                    cx.notify();
                }))
                .into_any_element();
        }

        if self.held_back(&still) {
            let what = match shape {
                MediaShape::Video => "Video",
                MediaShape::Gif => "GIF",
                _ => "Image",
            };
            let bytes = attachment.file.bytes;
            return self.load_box(tile, &still, what, bytes, width >= 160., cx).child(edge_line()).into_any_element();
        }

        let natural = natural(attachment);
        let edge = media::decode_edge(natural, width, height, fit, window.scale_factor());
        let size = media::fetch_size(natural, width, height, fit, window.scale_factor());
        let crop = (fit == Fit::CoverTop).then_some(height / width);
        // Whether the picture moves, and whether its frames are here to move in place.
        let (mut moves, mut frames) = (false, false);
        // A picture smaller than its box sits in the middle at its own size. Its preview takes
        // the same place, so the sharper picture lands exactly where the soft one was.
        let (wide, tall) = match fit {
            Fit::Contain if natural.0 > 0 && natural.1 > 0 => {
                let scale = (width / natural.0 as f32).min(height / natural.1 as f32).min(1.);
                ((natural.0 as f32 * scale).round(), (natural.1 as f32 * scale).round())
            }
            _ => (width, height),
        };
        let placed = |tile: Stateful<Div>, image, object_fit| {
            tile.child(
                div().absolute().inset_0().flex().items_center().justify_center().child(
                    div().w(px(wide)).h(px(tall)).flex_none().child(anim::picture(image, object_fit, theme::R_CARD, Play::Inline)),
                ),
            )
        };
        let tile = match self.still(&still, size, edge, crop, cx) {
            Picture::Ready(image) => {
                let object_fit = if fit == Fit::Contain { ObjectFit::Contain } else { ObjectFit::Cover };
                frames = image.frame_count() > 1;
                moves = self.images.moves(&image);
                placed(tile, image, object_fit)
            }
            // Until the still is decoded, the few pixels the message carried stand in for it,
            // stretched over its whole place.
            Picture::Loading => match self.preview(attachment, crop, cx) {
                Some(preview) => placed(tile, preview, ObjectFit::Cover),
                None => tile.child(in_box(Icon::Image, None)),
            },
            Picture::Failed => {
                let tile = self.failed_box(tile, &still, height >= 100. && width >= 180., cx);
                return tile.child(edge_line()).into_any_element();
            }
        };

        // A video, or a GIF that is really an MP4: what says it moves, and what a click does,
        // are `video`'s (`Pane::moving`, where an inline player plugs in). A GIF that is a GIF
        // plays here in its tile, and opens in the viewer like a picture.
        let id = message.id.clone();
        let page = attachment.page.clone();
        let fav_file = attachment.file.id.clone();
        let discord_gif = shape == MediaShape::Gif
            && (page.starts_with("https://") || page.starts_with("http://"))
            && self.open.as_ref().is_some_and(|open| open.key.network == protocol::Network::Discord);
        let tile = match shape {
            MediaShape::Video => self.moving(message, attachment, tile, cx),
            MediaShape::Gif if media::is_video(attachment) => self.moving(message, attachment, tile, cx),
            _ => {
                // A GIF says so where it does not play by itself. One held still (the setting
                // is Never, the network sent a still, or there was no room) shows that a click
                // plays it: the viewer does.
                let tile = match (shape == MediaShape::Gif || moves, frames) {
                    (true, false) => tile.child(play_disc()).child(badge("GIF".to_owned()).left(px(6.))),
                    (true, true) if anim::badged(Play::Inline) => tile.child(badge("GIF".to_owned()).left(px(6.))),
                    _ => tile,
                };
                // A described picture says so, and says what on hover.
                let tile = match attachment.alt.is_empty() {
                    true => tile,
                    false => tile.tooltip(tip(attachment.alt.clone(), None)).child(badge("ALT".to_owned()).right(px(6.))),
                };
                tile.on_click(cx.listener(move |this, _, window, cx| this.open_viewer(&id, index, window, cx)))
            }
        };
        // The border covers the picture. The star is painted after it, so a click lands on the
        // star and does not open the GIF. A "+N" cover, when there is one, stays on top.
        let tile = tile.child(edge_line());
        let tile = if discord_gif {
            let star_id: SharedString = format!("gif-fav-{fav_file}").into();
            tile.child(
                div()
                    .id(star_id)
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .size(px(24.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(gpui::rgba(0x00000099))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tooltip(tip("Favourite GIF", None))
                    .child(glyph("icons/star.svg", 14., gpui::white()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.favourite_posted_gif(&page, cx);
                    })),
            )
        } else {
            tile
        };
        let tile = tile.children(more.map(|more| {
            div()
                .absolute()
                .inset_0()
                .rounded(px(theme::R_CARD))
                .bg(gpui::rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .type_headline()
                .text_color(gpui::white())
                .child(format!("+{more}"))
        }));
        tile.into_any_element()
    }

    /// A sticker: its own size, no box, no border, and whatever it leaves clear shows the row.
    fn sticker(&mut self, message: &Rc<Message>, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let attachment = &message.attachments[index];
        let still = attachment.thumb.as_ref().unwrap_or(&attachment.file).clone();
        let edge = media::square_size(128., window.scale_factor());
        let sticker = div().id(("sticker", index)).size(px(128.)).flex_none().relative();
        if self.held_back(&still) {
            return self.load_box(sticker.rounded(px(theme::R_CARD)).bg(tokens::media_fill()), &still, "Sticker", 0, false, cx).into_any_element();
        }
        match self.still(&still, edge, edge, None, cx) {
            Picture::Ready(image) => sticker.child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline)),
            Picture::Loading => sticker.child(in_box(Icon::Image, None)),
            Picture::Failed => self.failed_box(sticker, &still, false, cx),
        }
        .into_any_element()
    }

    fn file_row(&mut self, message: &Rc<Message>, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let attachment = &message.attachments[index];
        if crate::gui::app::prefs::choices().text_previews && !attachment.spoiler && !self.is_gated() {
            if let Some(lang) = super::text_preview::preview_language(attachment) {
                if let Some(card) = self.text_preview_card(message, index, lang, cx) {
                    return card;
                }
            }
        }
        let file = attachment.file.clone();
        let on_disk = file.path.clone();
        let fetching = self.then.contains_key(&file.id);
        let failed = file.failed.is_some();
        let reason = file.failed.clone();
        let mut meta = media::type_label(attachment);
        if file.bytes > 0 {
            meta = format!("{meta} · {}", media::size_label(file.bytes));
        }
        let (meta, tone) = match (failed, fetching, on_disk.is_some()) {
            (true, ..) => ("Couldn't download · Retry".to_owned(), theme::danger()),
            (_, true, _) => (format!("{meta} · Downloading…"), theme::text_faint()),
            (_, _, true) => (format!("{meta} · Downloaded"), theme::text_faint()),
            _ => (meta, theme::text_faint()),
        };
        let look = attachment.clone();
        let phone = self.metrics == Metrics::PHONE;
        let row = div()
            .id(("file", index))
            .w(px(360.))
            .max_w_full()
            .h(px(if phone { 60. } else { 56. }))
            .pl(px(10.))
            .pr(px(8.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .hover(|style| style.bg(theme::over(theme::hover(), tokens::card())).border_color(tokens::border_strong()))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when_some(reason, |row, reason| row.tooltip(tip(format!("{reason} Click to try again."), None)))
            .on_click(cx.listener(move |this, _, _, cx| this.fetch_then(&look, Then::QuickLook, cx)))
            .child(
                div()
                    .size(px(36.))
                    .flex_none()
                    .rounded(px(theme::R_CARD))
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(media::file_icon(attachment), 18., theme::text_soft())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .type_ui()
                            .when(phone, |name| name.text_size(px(tokens_phone::scaled(15.))).line_height(px(tokens_phone::scaled(20.))))
                            .font_weight(theme::MEDIUM)
                            .child(attachment.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .type_micro()
                            .when(phone, |meta| meta.text_size(px(tokens_phone::scaled(12.))).line_height(px(tokens_phone::scaled(16.))))
                            .font_weight(theme::REGULAR)
                            .text_color(tone)
                            .child(meta),
                    ),
            );
        // A phone has no folder to show a file in: a tap opens it, and the one button downloads
        // or opens it, at a thumb's size.
        if phone {
            let open = attachment.clone();
            let which = if on_disk.is_some() { Icon::External } else { Icon::Download };
            return row
                .child(tool("open", which, 40.).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.fetch_then(&open, Then::Open, cx);
                })))
                .into_any_element();
        }
        let row = match on_disk {
            Some(path) => {
                let (open, reveal) = (attachment.clone(), path);
                row.child(
                    tool("open", Icon::External, 28.)
                        .tooltip(tip("Open", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.fetch_then(&open, Then::Open, cx);
                        })),
                )
                .child(
                    tool("reveal", Icon::Folder, 28.)
                        .tooltip(tip(crate::host::REVEAL, None))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.reveal_path(std::path::Path::new(&reveal));
                        })),
                )
            }
            None => row.child(tool("download", Icon::Download, 28.).tooltip(tip("Download", None))),
        };
        row.into_any_element()
    }

    /// Inline preview card for a text or code file attachment (#385). Fetches up to 100 KB via a
    /// ranged request only when this row is drawn on screen, decodes UTF-8 lossily off the UI
    /// thread, rejects binary-looking bytes, and renders with the same syntax-coloured code frame.
    fn text_preview_card(
        &mut self,
        message: &Rc<Message>,
        index: usize,
        language: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let attachment = message.attachments[index].clone();
        let file_id = attachment.file.id.clone();
        let preview = match self.text_previews.get(&file_id) {
            Some(Some(preview)) => Rc::clone(preview),
            Some(None) => return None,
            None => {
                let head_key = super::text_preview::head_id(&file_id);
                let disk_path = attachment
                    .file
                    .path
                    .clone()
                    .or_else(|| self.known.get(&file_id).cloned())
                    .or_else(|| self.known.get(&head_key).cloned());
                if let Some(path) = disk_path {
                    if self.text_preview_loading.insert(file_id.clone()) {
                        let declared = attachment.file.bytes;
                        let id_for_task = file_id.clone();
                        let path_buf = std::path::PathBuf::from(path);
                        cx.spawn(async move |this, cx| {
                            let decoded = cx
                                .background_executor()
                                .spawn(async move {
                                    super::text_preview::read_file_preview(&path_buf, declared, language)
                                })
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                this.text_preview_loading.remove(&id_for_task);
                                this.text_previews.insert(id_for_task, decoded.map(Rc::new));
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                } else if attachment.file.failed.is_none() && !self.asked_text_previews.contains(&file_id) {
                    self.want_text_previews.push(file_id);
                }
                return None;
            }
        };

        let block_id = 10_000usize.saturating_add(index);
        let key = (message.id.clone(), block_id);
        let lines = preview.inline_lines;
        let folded = lines > CODE_LINES && !self.expanded.contains(&key);
        let lit = if lines > super::highlight::PLAIN_LINES {
            None
        } else {
            self.code_colors(message, block_id, preview.language, &preview.inline_text, cx)
        };
        let shown = match &lit {
            Some(lit) if folded => first_lines(&lit.text, CODE_LINES).to_owned(),
            Some(lit) => lit.text.clone(),
            None if folded => preview.inline_text.lines().take(CODE_LINES).collect::<Vec<_>>().join("\n"),
            None => preview.inline_text.clone(),
        };
        let highlights = code_highlights(&shown, lit.as_ref().map(|lit| lit.runs.as_slice()).unwrap_or(&[]));
        let copied = self.copied.as_ref() == Some(&key);
        let phone = self.metrics == Metrics::PHONE;
        let btn_size = if phone { 36. } else { 24. };
        let copy_text = preview.head_text.clone();
        let copy_key = key.clone();
        let expand_att = attachment.clone();
        let expand_text = preview.head_text.clone();
        let expand_truncated = preview.truncated;
        let expand_lang = preview.language;
        let download_att = attachment.clone();
        let mut meta = media::type_label(&attachment);
        if attachment.file.bytes > 0 {
            meta = format!("{meta} · {}", media::size_label(attachment.file.bytes));
        }
        let mut body = div().id(("text-preview-body", block_id)).overflow_x_scroll().px(px(12.)).pt(px(2.)).pb(px(9.));
        body.style().restrict_scroll_to_axis = Some(true);

        let footer = if folded {
            let expand_key = key.clone();
            Some(
                div()
                    .id(("text-preview-more", block_id))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .type_small()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .child(format!("Show {} more lines", lines - CODE_LINES))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded.insert(expand_key.clone());
                        cx.notify();
                    })),
            )
        } else if preview.truncated {
            let open_att = attachment.clone();
            let open_text = preview.head_text.clone();
            let open_lang = preview.language;
            Some(
                div()
                    .id(("text-preview-full", block_id))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .type_small()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .child("Expand full file")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_text_viewer(open_att.clone(), open_lang, open_text.clone(), true, window, cx);
                    })),
            )
        } else {
            None
        };

        Some(
            div()
                .mt(px(5.))
                .mb(px(3.))
                .max_w(px(MEASURE))
                .rounded(px(theme::R_CARD))
                .bg(tokens::code_bg())
                .border_1()
                .border_color(theme::hairline())
                .overflow_hidden()
                .child(
                    div()
                        .h(px(if phone { 38. } else { 32. }))
                        .pl(px(10.))
                        .pr(px(4.))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(icon(media::file_icon(&attachment), 14., theme::text_soft()))
                                .child(div().min_w_0().truncate().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child(attachment.name.clone()))
                                .child(div().flex_none().type_micro().text_color(theme::text_faint()).child(meta)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .child(
                                    tool(("tp-copy", block_id), if copied { Icon::Check } else { Icon::Copy }, btn_size)
                                        .when(!phone, |b| b.tooltip(tip("Copy text", None)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                            this.copied = Some(copy_key.clone());
                                            cx.notify();
                                            let copy_key = copy_key.clone();
                                            cx.spawn(async move |this, cx| {
                                                cx.background_executor().timer(Duration::from_millis(1500)).await;
                                                let _ = this.update(cx, |this, cx| {
                                                    if this.copied.as_ref() == Some(&copy_key) {
                                                        this.copied = None;
                                                        cx.notify();
                                                    }
                                                });
                                            })
                                            .detach();
                                        })),
                                )
                                .child(
                                    tool(("tp-expand", block_id), Icon::Fit, btn_size)
                                        .when(!phone, |b| b.tooltip(tip("Expand", None)))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_text_viewer(
                                                expand_att.clone(),
                                                expand_lang,
                                                expand_text.clone(),
                                                expand_truncated,
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    tool(("tp-download", block_id), Icon::Download, btn_size)
                                        .when(!phone, |b| b.tooltip(tip("Download", None)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.fetch_then(&download_att, Then::Open, cx);
                                        })),
                                ),
                        ),
                )
                .child(crate::gui::components::scroll::scroll_content(
                    ("tp-scroll-frame", block_id),
                    body.pb(px(14.)).child(
                        div()
                            .type_code()
                            .when(phone, |code| code.text_size(px(tokens_phone::scaled(14.))).line_height(px(tokens_phone::scaled(20.))))
                            .whitespace_nowrap()
                            .child(if highlights.is_empty() {
                                shown.into_any_element()
                            } else {
                                StyledText::new(SharedString::from(shown)).with_highlights(highlights).into_any_element()
                            }),
                    ),
                ))
                .children(footer)
                .into_any_element(),
        )
    }

    /// What a bot or app posts instead of text: one framed card, its bar in one of our colors.
    /// A small picture inside a card (an icon, a thumbnail), once it is on disk. Nothing until then.
    fn card_picture(&mut self, file: &MediaFile, edge: f32, radius: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.held_back(file) {
            return None;
        }
        let size = media::decode_edge((file.width, file.height), edge, edge, Fit::Cover, window.scale_factor());
        let Picture::Ready(decoded) = self.still(file, size, size, None, cx) else {
            return None;
        };
        let shown = anim::picture(decoded, ObjectFit::Cover, radius, Play::Inline);
        Some(div().size(px(edge)).flex_none().rounded(px(radius)).overflow_hidden().child(shown).into_any_element())
    }

    /// A card's own words with their styles: a link opens on a click and a spoiler shows. Text
    /// here is not part of a selection.
    fn card_text(&mut self, message: &Message, ix: usize, card: &Card, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let key = format!("{}/card{ix}", message.id);
        let revealed: Vec<u32> = self.revealed.iter().filter(|(id, _)| *id == key).map(|(_, span)| *span).collect();
        let me = self.open.as_ref().map(|open| open.me.clone()).unwrap_or_default();
        let edge = media::square_size(18., window.scale_factor());
        let color = theme::text_soft().into_color();
        let mut out = Vec::new();
        for (at, block) in rich::blocks(&card.text, &card.text_spans).iter().enumerate() {
            let inline = match block {
                Block::Code { text, .. } => {
                    out.push(div().px(px(6.)).py(px(4.)).rounded(px(theme::R_CONTROL)).bg(tokens::code_bg()).type_code().child(text.clone()).into_any_element());
                    continue;
                }
                Block::Text(inline) | Block::Small(inline) | Block::Quote(inline) | Block::Heading { text: inline, .. } | Block::Item { text: inline, .. } => inline,
            };
            // What each run does on a click, read once: the handler outlives this draw.
            let acts: Vec<(Option<String>, Option<u32>)> = inline.runs.iter().map(|run| (run.link.clone(), run.spoiler)).collect();
            let (this, key) = (cx.weak_entity(), key.clone());
            let mut emoji = |file: &MediaFile| match self.still(file, edge, edge, None, cx) {
                Picture::Ready(image) => Some(image),
                _ => None,
            };
            let look = Look {
                color,
                hovered: None,
                revealed: &|span| revealed.contains(&span),
                me: &me,
                mentions_me: false,
                emoji: &mut emoji,
                selection: None,
                dragging: false,
                history: false,
            };
            let words = Rich::new(("card-text", ix * 256 + at), inline, look, move |event, window, cx| {
                let TextEvent::Click(run, alt) = event else { return };
                let Some((link, spoiler)) = acts.get(run).cloned() else { return };
                let _ = this.update(cx, |this, cx| {
                    if spoiler.is_some_and(|spoiler| this.revealed.insert((key.clone(), spoiler))) {
                        cx.notify();
                    } else if let Some(url) = link {
                        this.open_link(&url, alt, window, cx);
                    }
                });
            });
            let body = div().min_w_0().flex_1().flex().flex_col().type_ui().child(words);
            let marker = |words: String| div().w(px(20.)).flex_none().type_ui().text_color(theme::text_faint()).child(words);
            out.push(match block {
                Block::Heading { .. } => body.font_weight(theme::SEMIBOLD).into_any_element(),
                Block::Quote(_) => div().flex().gap(px(8.)).child(div().w(px(3.)).flex_none().rounded(px(2.)).bg(tokens::border_strong())).child(body).into_any_element(),
                Block::Item { ordered, depth, number, .. } => {
                    let words = if *ordered { format!("{number}.") } else { "\u{2022}".to_owned() };
                    div().pl(px(20. * f32::from(*depth))).flex().child(marker(words)).child(body).into_any_element()
                }
                _ => body.into_any_element(),
            });
        }
        out
    }

    fn app_card(&mut self, message: &Message, ix: usize, card: &Card, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let bar = match media::card_tone(card.color) {
            media::Tone::Positive => theme::positive(),
            media::Tone::Danger => theme::danger(),
            media::Tone::Warning => theme::warning(),
            media::Tone::Neutral => tokens::border_strong(),
        };
        let field = |(at, (label, value)): (usize, &(String, String))| {
            div()
                // A field that does not share its row takes the row.
                .map(|field| if media::card_field_inline(card, at) { field.w(px(190.)) } else { field.w_full() })
                .min_w_0()
                .flex()
                .flex_col()
                .children((!label.is_empty()).then(|| div().truncate().type_small().text_color(theme::text_faint()).child(label.clone())))
                .child(div().type_ui().child(value.clone()))
        };
        let maps_url = (card.footer == "Open in Maps").then(|| maps_url_for_card(card)).flatten();
        let vcard_path = (card.footer == "Contact card").then(|| {
            message
                .attachments
                .iter()
                .find(|a| a.mime.to_ascii_lowercase().contains("vcard") || a.name.to_ascii_lowercase().ends_with(".vcf"))
                .and_then(|a| a.file.path.clone())
        }).flatten();
        let footer_el: Option<AnyElement> = if let Some(url) = maps_url {
            Some(
                div()
                    .mt(px(4.))
                    .flex()
                    .child(
                        div()
                            .id(SharedString::from(format!("card-maps-{}-{ix}", message.id)))
                            .cursor_pointer()
                            .type_small()
                            .font_weight(theme::MEDIUM)
                            .text_color(theme::accent())
                            .hover(|s| s.underline())
                            .child("Open in Maps")
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                cx.open_url(&url);
                            }),
                    )
                    .into_any_element(),
            )
        } else if card.footer == "Contact card" {
            let save_path = vcard_path.clone();
            let text_path = vcard_path.clone();
            let account = self.open.as_ref().map(|o| o.key.account());
            let mut row = div().mt(px(4.)).flex().items_center().gap(px(12.));
            if let (Some(path), Some(account)) = (text_path, account) {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("card-text-number-{}-{ix}", message.id)))
                        .cursor_pointer()
                        .type_small()
                        .font_weight(theme::MEDIUM)
                        .text_color(theme::accent())
                        .hover(|s| s.underline())
                        .child("Text this number")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(phone) = first_vcard_phone(&path) {
                                let person = protocol::Person::new(
                                    protocol::Key::new(&account, phone),
                                    String::new(),
                                );
                                let _ = this.shell.update(cx, |shell, cx| shell.start_direct(&person, cx));
                            }
                        })),
                );
            }
            if let Some(path) = save_path {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("card-save-vcard-{}-{ix}", message.id)))
                        .cursor_pointer()
                        .type_small()
                        .text_color(theme::text_soft())
                        .hover(|s| s.underline())
                        .child("Save file")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.reveal_path(std::path::Path::new(&path));
                        }),
                );
            } else {
                row = row.child(div().type_small().text_color(theme::text_faint()).child(card.footer.clone()));
            }
            Some(row.into_any_element())
        } else if !card.footer.is_empty() && !card.title_link.is_empty() && card.footer.starts_with("Open in ") {
            let url = card.title_link.clone();
            Some(
                div()
                    .mt(px(4.))
                    .flex()
                    .child(
                        div()
                            .id(SharedString::from(format!("card-open-{}-{ix}", message.id)))
                            .cursor_pointer()
                            .type_small()
                            .font_weight(theme::MEDIUM)
                            .text_color(theme::accent())
                            .hover(|s| s.underline())
                            .child(card.footer.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_external_link(&url, window, cx);
                            })),
                    )
                    .into_any_element(),
            )
        } else {
            let said = media::card_footer(card, |unix| time::stamp(crate::view::now(), unix));
            let icon = card.footer_icon.as_ref().and_then(|file| self.card_picture(file, 16., 8., window, cx));
            (!said.is_empty()).then(|| {
                let words = div().min_w_0().type_small().text_color(theme::text_faint()).child(said);
                let line = div().mt(px(2.)).flex().items_center().gap(px(6.)).children(icon).child(words);
                match card.time {
                    Some(unix) => line.id(SharedString::from(format!("card-time-{}-{ix}", message.id))).tooltip(tip(time::tooltip(unix), None)).into_any_element(),
                    None => line.into_any_element(),
                }
            })
        };
        let author_el = (!card.author.is_empty()).then(|| {
            let icon = card.author_icon.as_ref().and_then(|file| self.card_picture(file, 20., 10., window, cx));
            let name = div().min_w_0().truncate().type_small().font_weight(theme::MEDIUM).text_color(theme::text_soft()).child(card.author.clone());
            let line = div().flex().items_center().gap(px(6.)).children(icon).child(name);
            if card.author_link.is_empty() {
                return line.into_any_element();
            }
            let url = card.author_link.clone();
            line.id(SharedString::from(format!("card-author-{}-{ix}", message.id)))
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_external_link(&url, window, cx);
                }))
                .into_any_element()
        });
        let thumbnail_el = card.thumbnail.as_ref().and_then(|file| self.card_picture(file, 72., 6., window, cx));
        let text_el: Vec<AnyElement> = match card.text_spans.is_empty() {
            true => (!card.text.is_empty()).then(|| div().type_ui().text_color(theme::text_soft()).child(card.text.clone()).into_any_element()).into_iter().collect(),
            false => self.card_text(message, ix, card, window, cx),
        };
        let title_el = (!card.title.is_empty()).then(|| {
            if card.title_link.is_empty() {
                div().type_body().font_weight(theme::SEMIBOLD).child(card.title.clone()).into_any_element()
            } else {
                let url = card.title_link.clone();
                div()
                    .id(SharedString::from(format!("card-title-link-{}-{ix}", message.id)))
                    .type_body()
                    .font_weight(theme::SEMIBOLD)
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_external_link(&url, window, cx);
                    }))
                    .child(card.title.clone())
                    .into_any_element()
            }
        });
        let actions_el = (!card.actions.is_empty()).then(|| {
            let buttons = card.actions.iter().enumerate().map(|(a_ix, action)| {
                let url = action.url.clone();
                div()
                    .id(SharedString::from(format!("card-action-{}-{ix}-{a_ix}", message.id)))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(theme::R_CONTROL))
                    .border_1()
                    .border_color(theme::hairline())
                    .bg(theme::canvas())
                    .type_small()
                    .font_weight(theme::MEDIUM)
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()).underline())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    })
                    .child(action.label.clone())
            });
            div().mt(px(4.)).flex().flex_wrap().gap(px(6.)).children(buttons)
        });
        let words = div()
            .flex_1()
            .min_w_0()
            .py(px(10.))
            .px(px(12.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(author_el)
            .children(title_el)
            .children(text_el)
            .children((!card.fields.is_empty()).then(|| div().mt(px(2.)).flex().flex_wrap().gap_x(px(12.)).gap_y(px(6.)).children(card.fields.iter().enumerate().map(field))))
            .children(actions_el)
            .children(footer_el);
        let framed = div()
            // The column's width on a phone, so its wrapped words measure against it (`card`).
            .w(px(if self.metrics == Metrics::PHONE { self.media_width() } else { 440. }))
            .max_w_full()
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .flex()
            .child(div().w(px(3.)).flex_none().bg(bar))
            .child(words)
            .children(thumbnail_el.map(|picture| div().flex_none().pt(px(10.)).pr(px(12.)).child(picture)));
        div().pt(px(5.)).flex().child(framed).into_any_element()
    }

    /// Inline keyboard button rows under a bot message (Telegram `replyInlineMarkup`).
    fn message_buttons(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.buttons.is_empty() {
            return None;
        }
        let conversation = self.key()?.clone();
        let mut grid = div().pt(px(6.)).flex().flex_col().gap(px(4.)).max_w(px(360.));
        for (row_ix, buttons) in message.buttons.iter().enumerate() {
            let mut row = div().flex().items_center().gap(px(4.));
            for (col_ix, btn_spec) in buttons.iter().enumerate() {
                let pending_key = format!("btn:{row_ix}:{col_ix}");
                let pending = self
                    .pending_actions
                    .get(&(message.id.clone(), pending_key.clone()))
                    .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(15));
                let el_id = SharedString::from(format!("ibtn-{}-{row_ix}-{col_ix}", message.id));
                let label = match &btn_spec.kind {
                    protocol::InlineButtonKind::Callback { .. } if pending => format!("{}…", btn_spec.label),
                    protocol::InlineButtonKind::Url { .. } => format!("{} ↗", btn_spec.label),
                    protocol::InlineButtonKind::Copy { .. } => format!("{} ⧉", btn_spec.label),
                    _ => btn_spec.label.clone(),
                };
                let muted = matches!(btn_spec.kind, protocol::InlineButtonKind::Unsupported);
                let mut btn = div()
                    .id(el_id)
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(tokens::card())
                    .border_1()
                    .border_color(tokens::border_strong())
                    .flex()
                    .items_center()
                    .justify_center()
                    .type_small()
                    .font_weight(theme::MEDIUM)
                    .text_color(if muted { theme::text_disabled() } else { theme::text() })
                    .cursor_pointer()
                    .when(pending, |d| d.opacity(0.6))
                    .hover(|s| s.bg(theme::hover()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().truncate().child(label));
                match &btn_spec.kind {
                    protocol::InlineButtonKind::Callback { data, requires_password } => {
                        if *requires_password {
                            btn = btn.on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.say("This button requires your Telegram password. Open in Telegram.".to_owned(), cx);
                            }));
                        } else if !pending {
                            let conv = conversation.clone();
                            let msg_id = message.id.clone();
                            let data = data.clone();
                            let key = pending_key.clone();
                            btn = btn.on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.press_inline_button(conv.clone(), msg_id.clone(), key.clone(), data.clone(), window, cx);
                            }));
                        }
                    }
                    protocol::InlineButtonKind::Url { url } => {
                        let conv = conversation.clone();
                        let msg_id = message.id.clone();
                        let target = url.clone();
                        btn = btn.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.trigger_link_control(conv.clone(), msg_id.clone(), target.clone(), None, window, cx);
                        }));
                    }
                    protocol::InlineButtonKind::Copy { text } => {
                        let text = text.clone();
                        btn = btn.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                            this.say("Copied to clipboard.".to_owned(), cx);
                        }));
                    }
                    protocol::InlineButtonKind::SwitchInline { query, .. } => {
                        let query = query.clone();
                        btn = btn.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !query.is_empty() {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(query.clone()));
                                this.say("Inline query copied to clipboard.".to_owned(), cx);
                            }
                        }));
                    }
                    protocol::InlineButtonKind::Unsupported => {
                        btn = btn.on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.say("Open in Telegram to use this button.".to_owned(), cx);
                        }));
                    }
                }
                row = row.child(btn);
            }
            grid = grid.child(row);
        }
        Some(grid.into_any_element())
    }

    /// Users, roles, and channels the shell already holds, so an entity select with no stored
    /// options can still post a picked id.
    fn entity_choices(&self, element_type: &str, cx: &mut Context<Self>) -> Vec<protocol::ControlOption> {
        let Some(shell) = self.shell.upgrade() else {
            return Vec::new();
        };
        let shell = shell.read(cx);
        let Some(open) = self.open.as_ref() else {
            return Vec::new();
        };
        let account = open.key.account();
        let mut rows = Vec::new();
        if matches!(element_type, "discord_user_select" | "discord_mentionable_select") {
            for person in shell.people_state.hub().friends(&account) {
                if person.name.is_empty() || person.key.id.is_empty() {
                    continue;
                }
                rows.push(protocol::ControlOption {
                    label: person.name,
                    value: person.key.id,
                    description: String::new(),
                });
            }
        }
        if element_type == "discord_channel_select" {
            let space = shell
                .snapshot
                .as_ref()
                .and_then(|snap| snap.open.as_ref())
                .filter(|row| row.key == open.key)
                .and_then(|row| row.space.clone());
            if let Some(space) = space {
                if let Some(tree) = shell.trees.get(&Key::new(&account, space.as_str())) {
                    for conv in &tree.conversations {
                        if matches!(
                            conv.kind,
                            protocol::ConversationKind::Thread
                                | protocol::ConversationKind::Direct
                                | protocol::ConversationKind::Group
                        ) {
                            continue;
                        }
                        let name = conv.title.trim_start_matches('#').trim();
                        if name.is_empty() || conv.key.id.is_empty() {
                            continue;
                        }
                        rows.push(protocol::ControlOption {
                            label: name.to_owned(),
                            value: conv.key.id.clone(),
                            description: String::new(),
                        });
                    }
                }
            }
        }
        rows.truncate(100);
        rows
    }

    /// Interactive controls attached to a message (buttons, menus, pickers, radios, checkboxes).
    fn message_controls(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.controls.is_empty() {
            return None;
        }
        let conversation = self.key()?.clone();
        let mut container = div().pt(px(6.)).flex().flex_col().gap(px(6.));
        let mut current_row_id: Option<u8> = None;
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.));
        for (idx, control) in message.controls.iter().enumerate() {
            if let Some(prev) = current_row_id {
                if prev != control.row {
                    container = container.child(row);
                    row = div().flex().flex_wrap().items_center().gap(px(6.));
                }
            }
            current_row_id = Some(control.row);
            let pending = self
                .pending_actions
                .get(&(message.id.clone(), control.action_id.clone()))
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(10));
            let disabled = control.disabled;
            let el_id = SharedString::from(format!("ctrl-{}-{idx}", message.id));
            match &control.kind {
                protocol::ControlKind::Button { url, style } => {
                    let base = if control.emoji.is_empty() {
                        control.label.clone()
                    } else if control.label.is_empty() {
                        control.emoji.clone()
                    } else {
                        format!("{} {}", control.emoji, control.label)
                    };
                    let label = if pending {
                        format!("{base}…")
                    } else if !url.is_empty() {
                        format!("{base} ↗")
                    } else {
                        base
                    };
                    let (bg, fg, border, hover_bg) = match style {
                        protocol::ControlStyle::Primary => (theme::accent(), theme::on_accent(), theme::accent(), theme::accent()),
                        protocol::ControlStyle::Success => (theme::online(), theme::on_accent(), theme::online(), theme::online()),
                        protocol::ControlStyle::Danger => (theme::danger(), theme::on_accent(), theme::danger(), theme::danger()),
                        protocol::ControlStyle::Default => (tokens::card(), theme::text(), tokens::border_strong(), theme::hover()),
                    };
                    let mut btn = div()
                        .id(el_id)
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(bg)
                        .border_1()
                        .border_color(border)
                        .flex()
                        .items_center()
                        .type_small()
                        .font_weight(theme::MEDIUM)
                        .text_color(fg)
                        .when(!disabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover_bg)))
                        .when(disabled, |d| d.cursor_default().opacity(0.45))
                        .when(pending && !disabled, |d| d.opacity(0.6))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(label);
                    if disabled {
                        // Disabled button: drawn dimmed and inert.
                    } else if !url.is_empty() {
                        let conv = conversation.clone();
                        let msg_id = message.id.clone();
                        let target = url.clone();
                        let confirm = control.confirm.clone();
                        btn = btn.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.trigger_link_control(conv.clone(), msg_id.clone(), target.clone(), confirm.clone(), window, cx);
                        }));
                    } else if !pending {
                        let conv = conversation.clone();
                        let msg_id = message.id.clone();
                        let block_id = control.block_id.clone();
                        let action_id = control.action_id.clone();
                        let element_type = control.element_type.clone();
                        let value = control.value.clone();
                        let confirm = control.confirm.clone();
                        btn = btn.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.trigger_control(
                                conv.clone(),
                                msg_id.clone(),
                                block_id.clone(),
                                action_id.clone(),
                                element_type.clone(),
                                value.clone(),
                                Vec::new(),
                                confirm.clone(),
                                window,
                                cx,
                            );
                        }));
                    }
                    row = row.child(btn);
                }
                protocol::ControlKind::Select { placeholder, options, selected, external } => {
                    let entity = matches!(
                        control.element_type.as_str(),
                        "discord_user_select"
                            | "discord_role_select"
                            | "discord_mentionable_select"
                            | "discord_channel_select"
                    );
                    let menu_options = if !options.is_empty() {
                        options.clone()
                    } else if entity {
                        self.entity_choices(&control.element_type, cx)
                    } else {
                        Vec::new()
                    };
                    let current_label = selected
                        .as_deref()
                        .and_then(|val| menu_options.iter().find(|o| o.value == val).map(|o| o.label.as_str()))
                        .unwrap_or_else(|| if placeholder.is_empty() { "Select…" } else { placeholder.as_str() });
                    let shown = if pending {
                        format!("{current_label}…")
                    } else {
                        format!("{current_label} ▾")
                    };
                    let mut chip = div()
                        .id(el_id)
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(tokens::card())
                        .border_1()
                        .border_color(tokens::border_strong())
                        .flex()
                        .items_center()
                        .type_small()
                        .text_color(theme::text())
                        .when(!disabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                        .when(disabled, |d| d.cursor_default().opacity(0.45))
                        .when(pending && !disabled, |d| d.opacity(0.6))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(shown);
                    if disabled {
                        // Disabled select: inert.
                    } else if !menu_options.is_empty() && !pending {
                        let initial_multi = selected.iter().cloned().collect();
                        let open = super::OpenSelect {
                            conversation: conversation.clone(),
                            message_id: message.id.clone(),
                            block_id: control.block_id.clone(),
                            action_id: control.action_id.clone(),
                            element_type: control.element_type.clone(),
                            options: menu_options,
                            selected: selected.clone(),
                            multi_selected: initial_multi,
                            min_values: control.min_values,
                            max_values: control.max_values,
                            confirm: control.confirm.clone(),
                            at: gpui::Point::default(),
                        };
                        chip = chip.on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                            cx.stop_propagation();
                            let mut next = open.clone();
                            next.at = event.position();
                            this.open_select = Some(next);
                            cx.notify();
                        }));
                    } else if !entity && *external && !message.permalink.is_empty() {
                        let link = message.permalink.clone();
                        chip = chip.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_external_link(&link, window, cx);
                        }));
                    }
                    row = row.child(chip);
                }
                protocol::ControlKind::DatePicker { placeholder, initial } => {
                    let label = initial
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(if placeholder.is_empty() { "Pick a date" } else { placeholder.as_str() });
                    let shown = if pending { format!("📅 {label}…") } else { format!("📅 {label} ▾") };
                    let now = crate::view::now();
                    let mut opts = Vec::new();
                    if let Some(init) = initial.as_ref().filter(|s| !s.is_empty()) {
                        opts.push(protocol::ControlOption {
                            label: format!("Keep ({init})"),
                            value: init.clone(),
                            description: String::new(),
                        });
                    }
                    for (name, days) in [("Today", 0i64), ("Tomorrow", 1), ("In 3 days", 3), ("In 7 days", 7)] {
                        let ymd = format_ymd(now + days * 86_400);
                        if !opts.iter().any(|o| o.value == ymd) {
                            opts.push(protocol::ControlOption {
                                label: format!("{name} ({ymd})"),
                                value: ymd,
                                description: String::new(),
                            });
                        }
                    }
                    let open = super::OpenSelect {
                        conversation: conversation.clone(),
                        message_id: message.id.clone(),
                        block_id: control.block_id.clone(),
                        action_id: control.action_id.clone(),
                        element_type: control.element_type.clone(),
                        options: opts,
                        selected: initial.clone(),
                        multi_selected: Vec::new(),
                        min_values: 0,
                        max_values: 0,
                        confirm: control.confirm.clone(),
                        at: gpui::Point::default(),
                    };
                    row = row.child(
                        div()
                            .id(el_id)
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(tokens::card())
                            .border_1()
                            .border_color(tokens::border_strong())
                            .flex()
                            .items_center()
                            .type_small()
                            .text_color(theme::text())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                cx.stop_propagation();
                                let mut next = open.clone();
                                next.at = event.position();
                                this.open_select = Some(next);
                                cx.notify();
                            }))
                            .child(shown),
                    );
                }
                protocol::ControlKind::TimePicker { placeholder, initial } => {
                    let label = initial
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(if placeholder.is_empty() { "Pick a time" } else { placeholder.as_str() });
                    let shown = if pending { format!("🕒 {label}…") } else { format!("🕒 {label} ▾") };
                    let mut opts = Vec::new();
                    if let Some(init) = initial.as_ref().filter(|s| !s.is_empty()) {
                        opts.push(protocol::ControlOption {
                            label: init.clone(),
                            value: init.clone(),
                            description: String::new(),
                        });
                    }
                    for slot in ["09:00", "12:00", "14:00", "17:00"] {
                        if !opts.iter().any(|o| o.value == slot) {
                            opts.push(protocol::ControlOption {
                                label: slot.to_owned(),
                                value: slot.to_owned(),
                                description: String::new(),
                            });
                        }
                    }
                    let open = super::OpenSelect {
                        conversation: conversation.clone(),
                        message_id: message.id.clone(),
                        block_id: control.block_id.clone(),
                        action_id: control.action_id.clone(),
                        element_type: control.element_type.clone(),
                        options: opts,
                        selected: initial.clone(),
                        multi_selected: Vec::new(),
                        min_values: 0,
                        max_values: 0,
                        confirm: control.confirm.clone(),
                        at: gpui::Point::default(),
                    };
                    row = row.child(
                        div()
                            .id(el_id)
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(tokens::card())
                            .border_1()
                            .border_color(tokens::border_strong())
                            .flex()
                            .items_center()
                            .type_small()
                            .text_color(theme::text())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                cx.stop_propagation();
                                let mut next = open.clone();
                                next.at = event.position();
                                this.open_select = Some(next);
                                cx.notify();
                            }))
                            .child(shown),
                    );
                }
                protocol::ControlKind::Checkboxes { options, selected } => {
                    for (opt_ix, opt) in options.iter().enumerate() {
                        let checked = selected.iter().any(|v| v == &opt.value);
                        let mut next = selected.clone();
                        if checked {
                            next.retain(|v| v != &opt.value);
                        } else {
                            next.push(opt.value.clone());
                        }
                        let conv = conversation.clone();
                        let msg_id = message.id.clone();
                        let block_id = control.block_id.clone();
                        let action_id = control.action_id.clone();
                        let element_type = control.element_type.clone();
                        let confirm = control.confirm.clone();
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("ctrl-chk-{}-{idx}-{opt_ix}", message.id)))
                                .h(px(26.))
                                .px(px(8.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(if checked { theme::accent_soft() } else { tokens::card() })
                                .border_1()
                                .border_color(if checked { theme::accent() } else { tokens::border_strong() })
                                .flex()
                                .items_center()
                                .type_small()
                                .text_color(theme::text())
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.trigger_control(
                                        conv.clone(),
                                        msg_id.clone(),
                                        block_id.clone(),
                                        action_id.clone(),
                                        element_type.clone(),
                                        String::new(),
                                        next.clone(),
                                        confirm.clone(),
                                        window,
                                        cx,
                                    );
                                }))
                                .child(format!("{} {}", if checked { "☑" } else { "☐" }, opt.label)),
                        );
                    }
                }
                protocol::ControlKind::RadioButtons { options, selected } => {
                    for (opt_ix, opt) in options.iter().enumerate() {
                        let chosen = selected.as_deref() == Some(opt.value.as_str());
                        let conv = conversation.clone();
                        let msg_id = message.id.clone();
                        let block_id = control.block_id.clone();
                        let action_id = control.action_id.clone();
                        let element_type = control.element_type.clone();
                        let val = opt.value.clone();
                        let confirm = control.confirm.clone();
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("ctrl-rad-{}-{idx}-{opt_ix}", message.id)))
                                .h(px(26.))
                                .px(px(8.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(if chosen { theme::accent_soft() } else { tokens::card() })
                                .border_1()
                                .border_color(if chosen { theme::accent() } else { tokens::border_strong() })
                                .flex()
                                .items_center()
                                .type_small()
                                .text_color(theme::text())
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.trigger_control(
                                        conv.clone(),
                                        msg_id.clone(),
                                        block_id.clone(),
                                        action_id.clone(),
                                        element_type.clone(),
                                        val.clone(),
                                        Vec::new(),
                                        confirm.clone(),
                                        window,
                                        cx,
                                    );
                                }))
                                .child(format!("{} {}", if chosen { "◉" } else { "○" }, opt.label)),
                        );
                    }
                }
            }
        }
        Some(container.child(row).into_any_element())
    }

    /// Shared contact card: name, organization, masked phone numbers (click to reveal), emails, and copy actions.
    fn contact_card(&self, message: &Rc<Message>, contact: &protocol::SharedContact, idx: usize, cx: &mut Context<Self>) -> AnyElement {
        let title = if contact.name.trim().is_empty() { "Shared contact" } else { contact.name.trim() };
        let mut rows: Vec<AnyElement> = Vec::new();
        let streamer = crate::gui::streamer::is_active();
        for (p_idx, phone) in contact.phones.iter().enumerate() {
            let reveal_key = format!("{}:{idx}:{p_idx}", message.id);
            let revealed = !streamer && self.revealed_contacts.contains(&reveal_key);
            let shown = if revealed { phone.clone() } else { crate::gui::people::mask(phone) };
            let copy_val = phone.clone();
            let toggle_key = reveal_key.clone();
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(name_id(&format!("contact-phone-{}-{idx}-{p_idx}", message.id), ""))
                            .when(!streamer, |row| row.cursor_pointer().hover(|s| s.text_color(theme::text())))
                            .type_ui()
                            .text_color(theme::text_soft())
                            .when(!streamer, |row| {
                                row.on_click(cx.listener(move |this, _, _, cx| this.toggle_contact_reveal(toggle_key.clone(), cx)))
                            })
                            .child(if streamer || revealed { shown } else { format!("{shown} · Show") }),
                    )
                    .when(!streamer, |row| {
                        row.child(
                            div()
                                .id(name_id(&format!("contact-phone-copy-{}-{idx}-{p_idx}", message.id), ""))
                                .cursor_pointer()
                                .type_micro()
                                .text_color(theme::accent())
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_val.clone()));
                                }))
                                .child("Copy"),
                        )
                    })
                    .into_any_element(),
            );
        }
        for (e_idx, email) in contact.emails.iter().enumerate() {
            let copy_val = email.clone();
            let shown_email = if streamer { crate::gui::people::mask_email(email) } else { email.clone() };
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().type_ui().text_color(theme::text_soft()).child(shown_email))
                    .when(!streamer, |row| {
                        row.child(
                            div()
                                .id(name_id(&format!("contact-email-copy-{}-{idx}-{e_idx}", message.id), ""))
                                .cursor_pointer()
                                .type_micro()
                                .text_color(theme::accent())
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_val.clone()));
                                }))
                                .child("Copy"),
                        )
                    })
                    .into_any_element(),
            );
        }
        let message_btn = contact.person_id.as_ref().and_then(|pid| {
            let open = self.open.as_ref()?;
            let person = protocol::Person::new(
                protocol::Key {
                    network: open.key.network,
                    account: open.key.account.clone(),
                    id: pid.clone(),
                },
                title.to_owned(),
            );
            Some(
                div()
                    .pt(px(4.))
                    .child(
                        div()
                            .id(name_id(&format!("contact-message-{}-{idx}", message.id), ""))
                            .cursor_pointer()
                            .type_micro()
                            .text_color(theme::accent())
                            .hover(|s| s.underline())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.shell.update(cx, |shell, cx| shell.start_direct(&person, cx));
                            }))
                            .child("Message"),
                    ),
            )
        });
        div()
            .mt(px(4.))
            .max_w(px(340.))
            .p(px(10.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().type_micro().text_color(theme::text_faint()).child("Contact"))
            .child(div().type_body().font_weight(theme::SEMIBOLD).child(title.to_owned()))
            .children((!contact.organization.is_empty()).then(|| div().type_ui().text_color(theme::text_soft()).child(contact.organization.clone())))
            .children(rows)
            .children(message_btn)
            .into_any_element()
    }

    /// Interactive poll card: question, choices with bar & count, my selection, Show results, End poll.
    fn poll_card(&self, message: &Rc<Message>, poll: &protocol::Poll, cx: &mut Context<Self>) -> AnyElement {
        let total = poll.total_votes();
        let ended = poll.closed || poll.closes_at.is_some_and(|expiry| expiry <= crate::view::now());
        let show_results = ended || !poll.mine.is_empty() || self.poll_results.contains(&message.id);
        let subtitle = {
            let mut parts: Vec<String> = Vec::new();
            if ended {
                parts.push("Poll closed".to_owned());
            } else if poll.multiple {
                parts.push("Select one or more".to_owned());
            } else {
                parts.push("Select one".to_owned());
            }
            if poll.anonymous {
                parts.push("Anonymous".to_owned());
            }
            if !ended {
                if let Some(closes_at) = poll.closes_at {
                    let left = closes_at.saturating_sub(crate::view::now());
                    if left <= 0 {
                        parts.push("Closing soon".to_owned());
                    } else if left < 3600 {
                        parts.push(format!("Closes in {}m", (left / 60).max(1)));
                    } else if left < 86_400 {
                        parts.push(format!("Closes in {}h", left / 3600));
                    } else {
                        parts.push(format!("Closes in {}d", left / 86_400));
                    }
                }
            }
            parts.join(" · ")
        };

        let mut rows = div().mt(px(6.)).flex().flex_col().gap(px(5.));
        for (idx, choice) in poll.choices.iter().enumerate() {
            let idx_u32 = idx as u32;
            let picked = poll.mine.contains(&idx_u32);
            let pct = poll_percentage(choice.count, total);
            let next = poll.next_votes(idx_u32);
            let msg_id = message.id.clone();
            let clickable = !ended && !self.poll_voting.contains(&message.id) && message.state == SendState::Sent;
            let emoji_path = choice.emoji_file.as_ref().and_then(|file| {
                let account = self.open.as_ref()?.key.account();
                let shell = self.shell.upgrade()?;
                let hub = shell.read(cx).people_state.hub().clone();
                hub.file(&account, file, cx).path.map(std::path::PathBuf::from)
            });
            let emoji = if let Some(path) = emoji_path {
                Some(gpui::img(path).flex_none().size(px(18.)).into_any_element())
            } else {
                (!choice.emoji.is_empty()).then(|| div().flex_none().child(choice.emoji.clone()).into_any_element())
            };

            let row = div()
                .id(name_id(&format!("poll-{}-{}", message.id, idx), ""))
                .relative()
                .overflow_hidden()
                .rounded(px(theme::R_CONTROL))
                .border_1()
                .border_color(if picked { theme::accent() } else { theme::hairline() })
                .bg(theme::canvas())
                .px(px(10.))
                .py(px(7.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .when(clickable, |el| {
                    el.cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.vote_poll(msg_id.clone(), Some(next.clone()), cx);
                        }))
                })
                .when(show_results && pct > 0, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .bottom_0()
                            .w(gpui::relative(pct as f32 / 100.))
                            .bg(if picked { theme::selected() } else { theme::hover() }),
                    )
                })
                .child(
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .flex_1()
                        .when(picked, |el| {
                            el.child(div().type_small().font_weight(theme::SEMIBOLD).text_color(theme::accent()).child("✓"))
                        })
                        .children(emoji)
                        .child(div().type_ui().truncate().text_color(theme::text()).child(choice.text.clone())),
                )
                .when(show_results, |el| {
                    el.child(
                        div()
                            .relative()
                            .flex_none()
                            .type_small()
                            .text_color(if picked { theme::accent() } else { theme::text_faint() })
                            .child(format!("{} ({pct}%)", choice.count)),
                    )
                });
            rows = rows.child(row);
            if show_results && choice.answer_id.is_some() && choice.count > 0 {
                let id = message.id.clone();
                rows = rows.child(button(("poll-voters-choice", idx), ButtonKind::Ghost, "View voters")
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_poll_voters(id.clone(), idx_u32, cx))));
            }
            if let Some(list) = self.poll_voters.as_ref().filter(|list| list.message_id == message.id && list.choice == idx_u32) {
                let mut voters = div().p(px(8.)).bg(theme::overlay()).rounded(px(theme::R_CONTROL)).flex().flex_col().gap(px(3.))
                    .child(div().type_small().font_weight(theme::SEMIBOLD).child(format!("Voters · {}", choice.text)))
                    .child(div().id(("poll-voters-list", idx)).max_h(px(180.)).overflow_y_scroll().children(
                        list.voters.iter().map(|voter| div().type_small().child(voter.name.clone())),
                    ));
                if list.loading {
                    voters = voters.child(div().type_small().child("Loading…"));
                } else if let Some(error) = &list.error {
                    voters = voters.child(div().type_small().text_color(theme::danger()).child(error.clone()));
                } else if list.voters.is_empty() {
                    voters = voters.child(div().type_small().child("No votes"));
                }
                if !list.loading && (list.more || list.error.is_some()) {
                    voters = voters.child(button(("poll-more-voters", idx), ButtonKind::Secondary, if list.error.is_some() { "Retry" } else { "Load more" })
                        .on_click(cx.listener(|this, _, _, cx| this.load_poll_voters(cx))));
                }
                rows = rows.child(voters);
            }
        }

        let vote_label = if total == 1 { "1 vote".to_owned() } else { format!("{total} votes") };
        let mut footer = div()
            .mt(px(6.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(div().type_small().text_color(theme::text_faint()).child(vote_label));

        let mut actions = div().flex().items_center().gap(px(8.));
        if !ended && poll.mine.is_empty() {
            let msg_id = message.id.clone();
            let toggle_label = if show_results { "Hide results" } else { "Show results" };
            actions = actions.child(
                div()
                    .id(name_id("poll-results", &message.id))
                    .type_small()
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_poll_results(msg_id.clone(), cx)))
                    .child(toggle_label),
            );
        }
        if message.outgoing && !ended && message.state == SendState::Sent && !self.poll_voting.contains(&message.id) {
            let msg_id = message.id.clone();
            actions = actions.child(
                div()
                    .id(name_id("poll-end", &message.id))
                    .type_small()
                    .text_color(theme::danger())
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |this, _, _, cx| this.vote_poll(msg_id.clone(), None, cx)))
                    .child("End poll"),
            );
        }
        footer = footer.child(actions);

        let body = div()
            .flex_1()
            .min_w_0()
            .py(px(10.))
            .px(px(12.))
            .flex()
            .flex_col()
            .child(div().type_body().font_weight(theme::SEMIBOLD).child(poll.question.clone()))
            .child(div().type_small().text_color(theme::text_faint()).child(subtitle))
            .child(rows)
            .child(footer);

        let framed = div()
            .w(px(if self.metrics == Metrics::PHONE { self.media_width() } else { 440. }))
            .max_w_full()
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .flex()
            .child(div().w(px(3.)).flex_none().bg(theme::accent()))
            .child(body);
        div().pt(px(5.)).flex().child(framed).into_any_element()
    }

    // Link cards.

    fn cards(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let previews: Vec<Preview> = message.previews.iter().take(CARDS).cloned().collect();
        let can_suppress = self.can(message).suppress_embeds;
        previews
            .iter()
            .enumerate()
            .map(|(ix, preview)| {
                let card = self.card(&message.id, ix, preview, can_suppress, window, cx);
                div().pt(px(if ix == 0 { 5. } else { 4. })).flex().child(card).into_any_element()
            })
            .collect()
    }

    fn card(
        &mut self,
        message_id: &str,
        index: usize,
        preview: &Preview,
        can_suppress: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview = self.wikipedia_preview(preview, cx);
        let scale = window.scale_factor();
        let image = preview.image.as_ref();
        // Only the first card may be large, and only for a picture wide enough to deserve it.
        // A site player is large even when its poster is small or missing.
        let large = preview.opens_in_browser
            || index == 0 && image.is_some_and(|image| image.width >= 400 && image.width as f32 >= image.height as f32 * 1.3);
        // A phone's card follows the phone's run: 12 / 16 / 14 over the desktop's 11 / 15 / 13.
        let phone = self.metrics == Metrics::PHONE;
        let sized = move |line: Div, size: f32, height: f32| match phone {
            true => line.text_size(px(tokens_phone::scaled(size))).line_height(px(tokens_phone::scaled(height))),
            false => line,
        };
        // Two lines at most. A phone holds them to two lines' height: the clamp's ellipsis is
        // measured past a narrow card's edge, and its last line ran out of the card.
        let clamp = move |line: Div, height: f32| match phone {
            true => line.max_h(px(2. * tokens_phone::scaled(height))).overflow_hidden(),
            false => line.line_clamp(2),
        };
        let words = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children((!preview.site.is_empty()).then(|| {
                sized(div().truncate().type_micro(), 12., 16.).font_weight(theme::REGULAR).text_color(theme::text_faint()).child(preview.site.clone())
            }))
            .children((!preview.title.is_empty()).then(|| {
                clamp(sized(div().type_body(), 16., 22.), 22.).font_weight(theme::SEMIBOLD).child(preview.title.clone())
            }))
            .children((!preview.description.is_empty()).then(|| {
                clamp(sized(div().type_ui(), 14., 20.), 20.).text_color(theme::text_soft()).child(preview.description.clone())
            }))
            .children(preview.opens_in_browser.then(|| {
                sized(div().truncate().type_micro(), 12., 16.).text_color(theme::text_faint()).child("Opens in browser")
            }));
        let url = preview.url.clone();
        let dismiss = can_suppress.then(|| {
            let msg_id = message_id.to_owned();
            div()
                .id(SharedString::from(format!("suppress-preview-{message_id}-{index}")))
                .absolute()
                .top(px(6.))
                .right(px(6.))
                .size(px(20.))
                .rounded(px(theme::R_CONTROL))
                .bg(tokens::card())
                .hover(|s| s.bg(theme::selected()))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .opacity(0.)
                .group_hover("link-card", |style| style.opacity(1.))
                .tooltip(tip("Remove link preview", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.suppress_message_embeds(&msg_id, cx);
                }))
                .child(glyph("icons/x.svg", 12., theme::text_soft()))
        });
        // A phone's card is exactly the column's width: wrapped words then measure against it,
        // not against the desktop's 440, and never run past its edge.
        let card = div()
            .id(("card", index))
            .group("link-card")
            .relative()
            .w(px(if phone { self.media_width() } else { self.media_width().min(440.) }))
            .max_w_full()
            .px(px(12.))
            .when(can_suppress, |c| c.pr(px(30.)))
            .py(px(10.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .hover(|style| style.bg(theme::over(theme::hover(), tokens::card())).border_color(tokens::border_strong()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| this.open_link(&url, event.modifiers().alt, window, cx)))
            .children(dismiss);
        let holder = |id: &'static str, wide: f32, tall: f32, radius: f32| {
            div().id(id).relative().w(px(wide)).h(px(tall)).flex_none().rounded(px(radius)).bg(theme::hover()).overflow_hidden()
        };
        let dress = |picture: Stateful<Div>| match preview.opens_in_browser {
            true => {
                let length = (preview.duration > 0).then(|| badge(media::duration_label(preview.duration)).right(px(6.)));
                picture.child(play_disc()).children(length)
            }
            false => picture,
        };
        let Some(image) = image.cloned() else {
            if !preview.opens_in_browser {
                return card.child(words).into_any_element();
            }
            let wide = if phone { self.media_width() - 26. } else { self.media_width().min(416.) };
            return card.flex().flex_col().gap(px(8.)).child(words).child(dress(holder("preview", wide, 180., 8.))).into_any_element();
        };
        let shown = |image, radius: f32| anim::picture(image, ObjectFit::Cover, radius, Play::Inline);
        if large {
            // On a phone the card is the column's width, so the picture is what its 12 pt
            // padding and 1 pt edges leave inside it.
            let wide = if self.metrics == Metrics::PHONE { self.media_width() - 26. } else { self.media_width().min(416.) };
            let tall = (wide * image.height as f32 / image.width.max(1) as f32).round().min(210.);
            let edge = media::decode_edge((image.width, image.height), wide, tall, Fit::Cover, scale);
            if self.held_back(&image) {
                let picture = self.load_box(holder("preview", wide, tall, 8.), &image, "Preview", image.bytes, true, cx);
                return card.flex().flex_col().gap(px(8.)).child(words).child(dress(picture)).into_any_element();
            }
            let picture = match self.still(&image, edge, edge, None, cx) {
                Picture::Ready(decoded) => holder("preview", wide, tall, 8.).child(shown(decoded, 8.)),
                Picture::Loading => holder("preview", wide, tall, 8.).child(in_box(Icon::Image, None)),
                Picture::Failed => self.failed_box(holder("preview", wide, tall, 8.), &image, false, cx),
            };
            return card.flex().flex_col().gap(px(8.)).child(words).child(dress(picture)).into_any_element();
        }
        let edge = media::decode_edge((image.width, image.height), 72., 72., Fit::Cover, scale);
        if self.held_back(&image) {
            let picture = self.load_box(holder("preview", 72., 72., 6.), &image, "Preview", 0, false, cx);
            return card.flex().gap(px(12.)).child(words).child(picture).into_any_element();
        }
        let picture = match self.still(&image, edge, edge, None, cx) {
            Picture::Ready(decoded) => holder("preview", 72., 72., 6.).child(shown(decoded, 6.)),
            Picture::Loading => holder("preview", 72., 72., 6.).child(in_box(Icon::Image, None)),
            Picture::Failed => self.failed_box(holder("preview", 72., 72., 6.), &image, false, cx),
        };
        card.flex().gap(px(12.)).child(words).child(picture).into_any_element()
    }

    /// Renders a Discord server invite card and/or scheduled event card under a message.
    fn invite_card(
        &mut self,
        message: &Message,
        ix: usize,
        invite: &MessageInvite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut space_name = invite.space_name.clone();
        let mut space_id = invite.space_id.clone();
        let mut icon_file = invite.icon.clone();
        let mut members = invite.members;
        let mut online = invite.online;
        let mut state = invite.state;
        let mut event: Option<ScheduledEvent> = invite.event.clone();
        let mut error_note: Option<String> = None;

        if !invite.code.is_empty() {
            let ev_id = invite.event.as_ref().map(|e| e.id.as_str()).filter(|id| !id.is_empty());
            let cache_key = match ev_id {
                Some(ev) => format!("{}:{ev}", invite.code),
                None => invite.code.clone(),
            };
            if let Some(cached) = self.invite_previews.get(&cache_key).cloned() {
                match cached {
                    Ok(preview) => {
                        space_name = preview.name;
                        space_id = preview.space_id;
                        icon_file = preview.icon;
                        members = preview.members;
                        online = preview.online;
                        state = if preview.joined { InviteState::Joined } else { InviteState::Valid };
                        if preview.event.is_some() {
                            event = preview.event;
                        }
                    }
                    Err(err) => {
                        state = InviteState::Invalid;
                        error_note = Some(err);
                    }
                }
            }
        } else if let Some(ev) = invite.event.as_ref().filter(|e| !e.guild_id.is_empty() && !e.id.is_empty()) {
            if let Some(cached) = self.event_previews.get(&(ev.guild_id.clone(), ev.id.clone())).cloned() {
                match cached {
                    Ok(resolved) => {
                        state = InviteState::Valid;
                        event = Some(resolved);
                    }
                    Err(err) => {
                        state = InviteState::Invalid;
                        error_note = Some(err);
                    }
                }
            }
        }

        let mut card = div()
            .id(name_id(&format!("invite-{ix}"), &message.id))
            .mt(px(6.))
            .max_w(px(432.))
            .p(px(12.))
            .rounded(px(theme::R_CARD))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .gap(px(10.));

        if !invite.code.is_empty() {
            let heading = match state {
                InviteState::Invalid => "INVITE INVALID",
                InviteState::Joined => "YOU'RE ALREADY IN THIS SERVER",
                _ => "YOU'VE BEEN INVITED TO JOIN A SERVER",
            };
            let title = if !space_name.is_empty() {
                space_name.clone()
            } else if state == InviteState::Invalid {
                "Invite invalid or expired".to_owned()
            } else {
                format!("discord.gg/{}", invite.code)
            };
            let counts = match (online, members) {
                (Some(on), Some(all)) => Some(format!("{on} online · {all} members")),
                (None, Some(all)) => Some(format!("{all} members")),
                (Some(on), None) => Some(format!("{on} online")),
                (None, None) => error_note.clone().or_else(|| (state == InviteState::Unresolved).then(|| "Loading invite…".to_owned())),
            };
            let place_id = space_id.as_deref().unwrap_or(invite.code.as_str());
            let place_key = self.open.as_ref().map(|open| Key::new(&open.key.account(), place_id));
            let tile = match &place_key {
                Some(key) => Avatar::new(key, &title, 40.),
                None => Avatar::named(&title, 40.),
            }
            .shape(Shape::Place);
            let avatar = self.face(tile, icon_file.as_ref(), 40., Shape::Place, window, cx);
            let code = invite.code.clone();
            let url = invite.url.clone();
            let valid_url = url.starts_with("https://") || url.starts_with("http://");
            let handoff = self.invite_handoff.contains(&code);
            let joining = self.invite_joining.contains(&code);

            let action: Option<AnyElement> = match state {
                InviteState::Joined => {
                    let target_space = space_id.clone();
                    Some(
                        div()
                            .id(name_id(&format!("invite-open-{ix}"), &message.id))
                            .flex_none()
                            .h(px(30.))
                            .px(px(12.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border())
                            .hover(|s| s.bg(theme::selected()))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .type_small()
                            .font_weight(theme::MEDIUM)
                            .text_color(theme::text())
                            .when_some(target_space, |btn, gid| {
                                btn.on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_invite_space(gid.clone(), cx);
                                }))
                            })
                            .child("Joined")
                            .into_any_element(),
                    )
                }
                InviteState::Valid => {
                    let label = if handoff {
                        "Open in Discord"
                    } else if joining {
                        "Joining…"
                    } else {
                        "Join"
                    };
                    Some(
                        div()
                            .id(name_id(&format!("invite-join-{ix}"), &message.id))
                            .flex_none()
                            .h(px(30.))
                            .px(px(14.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::accent())
                            .text_color(theme::on_accent())
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .type_small()
                            .font_weight(theme::SEMIBOLD)
                            .when(handoff && valid_url, |btn| {
                                let url = url.clone();
                                btn.on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_external_link(&url, window, cx);
                                }))
                            })
                            .when(!handoff && !joining, |btn| {
                                let code = code.clone();
                                btn.on_click(cx.listener(move |this, _, _, cx| {
                                    this.join_invite(code.clone(), cx);
                                }))
                            })
                            .child(label)
                            .into_any_element(),
                    )
                }
                InviteState::Invalid | InviteState::Unresolved => None,
            };

            card = card
                .child(div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::text_faint()).child(heading))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(avatar)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .truncate()
                                        .type_ui()
                                        .font_weight(theme::SEMIBOLD)
                                        .text_color(if state == InviteState::Invalid { theme::text_faint() } else { theme::text() })
                                        .child(title),
                                )
                                .children(counts.map(|c| div().truncate().type_micro().text_color(theme::text_soft()).child(c))),
                        )
                        .children(action),
                );
        }

        if let Some(ev) = event {
            let ev_title = if !ev.name.is_empty() {
                ev.name.clone()
            } else if state == InviteState::Invalid {
                "Event unavailable".to_owned()
            } else {
                "Scheduled Event".to_owned()
            };
            let when_text = ev.start.map(time::tooltip);
            let mut meta_parts: Vec<String> = Vec::new();
            if !ev.place.is_empty() {
                meta_parts.push(ev.place.clone());
            }
            if let Some(count) = ev.interested_count {
                meta_parts.push(format!("{count} interested"));
            }
            let meta_line = (!meta_parts.is_empty()).then(|| meta_parts.join(" · "));
            let can_interest = state != InviteState::Invalid && !ev.guild_id.is_empty() && !ev.id.is_empty();
            let toggling = self.event_toggling.contains(&(ev.guild_id.clone(), ev.id.clone()));
            let ev_guild = ev.guild_id.clone();
            let ev_id = ev.id.clone();
            let next_interested = !ev.interested;

            let mut ev_box = div()
                .when(!invite.code.is_empty(), |d| d.pt(px(8.)).border_t_1().border_color(theme::border()))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(when_text.map(|w| div().type_micro().font_weight(theme::SEMIBOLD).text_color(theme::accent_text()).child(w)))
                        .child(div().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(ev_title))
                        .children(meta_line.map(|m| div().truncate().type_micro().text_color(theme::text_soft()).child(m))),
                );
            if can_interest {
                ev_box = ev_box.child(
                    div()
                        .id(name_id(&format!("event-interest-{ix}"), &message.id))
                        .flex_none()
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(theme::R_CONTROL))
                        .border_1()
                        .when(ev.interested, |b| b.bg(theme::accent_soft()).border_color(theme::accent()).text_color(theme::accent_text()))
                        .when(!ev.interested, |b| b.bg(theme::raised()).border_color(theme::border()).text_color(theme::text()))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .type_small()
                        .font_weight(theme::MEDIUM)
                        .when(!toggling, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_event_interest(ev_guild.clone(), ev_id.clone(), next_interested, cx);
                            }))
                        })
                        .child(if ev.interested { "✓ Interested" } else { "Interested" }),
                );
            }
            card = card.child(ev_box);
        }

        card.into_any_element()
    }

    // Around the message.

    fn reactions(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.reactions.is_empty() {
            return None;
        }
        let edge = media::square_size(18., window.scale_factor());
        let acts = self.can(message).react;
        // A phone's chip is a thumb's target: 32 tall, its glyph and count a step larger.
        let phone = self.metrics == Metrics::PHONE;
        let mine_hover_bg = theme::over(theme::hover(), theme::over(theme::accent_soft(), theme::canvas()));
        let chip = |id: ElementId, mine: bool| {
            let chip = div()
                .id(id)
                .flex_none()
                .h(px(if phone { 32. } else { 25. }))
                .min_w(px(if phone { 48. } else { 0. }))
                .px(px(if phone { 10. } else { 7. }))
                .justify_center()
                .rounded_full()
                .border_1()
                .flex()
                .items_center()
                .gap(px(5.))
                .when(acts, |chip| chip.cursor_pointer());
            if mine {
                chip.bg(theme::accent_soft())
                    .border_color(theme::accent_halo())
                    .when(acts, move |chip| chip.hover(move |style| style.bg(mine_hover_bg).border_color(theme::accent())))
            } else {
                chip.bg(theme::raised())
                    .border_color(theme::hairline())
                    .when(acts, |chip| chip.hover(|style| style.bg(theme::hover()).border_color(tokens::border_strong())))
            }
        };
        let burst_chip = |id: ElementId, mine: bool, colors: &[String]| {
            let parsed = colors
                .first()
                .and_then(|s| s.strip_prefix('#').or(Some(s.as_str())))
                .filter(|s| s.len() == 6)
                .and_then(|hex| u32::from_str_radix(hex, 16).ok());
            let base = parsed.unwrap_or(0x8b5cf6);
            let accent = gpui::rgb(base);
            let fill = gpui::rgba((base << 8) | if mine { if theme::is_light() { 0x33 } else { 0x42 } } else if theme::is_light() { 0x1a } else { 0x24 });
            let edge_color = if mine { accent } else { gpui::rgba((base << 8) | 0x80) };
            (
                div()
                    .id(id)
                    .flex_none()
                    .h(px(if phone { 32. } else { 26. }))
                    .min_w(px(if phone { 48. } else { 0. }))
                    .px(px(if phone { 10. } else { 8. }))
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .bg(fill)
                    .border_color(edge_color)
                    .hover(move |style| style.border_color(accent))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .when(acts, |chip| chip.cursor_pointer()),
                accent,
            )
        };
        let mut row = div().mt(px(6.)).mb(px(3.)).flex().flex_wrap().gap(px(if phone { 8. } else { 6. }));
        // Flatten each reaction into its burst chip (if any) and normal chip (if any), so a
        // message with both normal and super reactions for the same emoji shows both distinctly.
        let mut entries: Vec<(usize, bool)> = Vec::new();
        for (ix, reaction) in message.reactions.iter().enumerate() {
            if reaction.burst > 0 {
                entries.push((ix, true));
            }
            if reaction.count > 0 {
                entries.push((ix, false));
            }
        }
        let total_chips = entries.len();
        let hidden_tip: Option<String> = (total_chips > CHIPS).then(|| {
            entries[CHIPS..]
                .iter()
                .map(|&(ix, is_burst)| {
                    let reaction = &message.reactions[ix];
                    let count = if is_burst { reaction.burst } else { reaction.count };
                    format!("{} {count}", emoji::display(&reaction.emoji))
                })
                .collect::<Vec<_>>()
                .join(" · ")
        });
        for (chip_ix, (ix, is_burst)) in entries.into_iter().take(CHIPS).enumerate() {
            let reaction = &message.reactions[ix];
            let picture = match &reaction.file {
                Some(file) => match self.still(file, edge, edge, None, cx) {
                    Picture::Ready(image) => Some(div().size(px(18.)).flex_none().child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline)).into_any_element()),
                    _ => None,
                },
                None => None,
            };
            // A custom emoji is its name until its image is here.
            let emoji = picture.unwrap_or_else(|| {
                let (size, line) = if phone { (17., 22.) } else { (15., 20.) };
                div().text_size(px(size)).line_height(px(line)).child(emoji::display(&reaction.emoji)).into_any_element()
            });
            let mine = if is_burst { reaction.mine_burst } else { reaction.mine };
            let shown_count = if is_burst { reaction.burst } else { reaction.count };
            // A custom one goes by its file, which names it where its `:name:` may not.
            let toggled = reaction.file.as_ref().map_or_else(|| reaction.emoji.clone(), |file| file.id.clone());
            let (id, on) = (message.id.clone(), !mine);
            let is_telegram = self.open.as_ref().is_some_and(|open| open.key.network == protocol::Network::Telegram);
            let is_paid_star = is_telegram && reaction.file.is_none() && protocol::bare_emoji(&reaction.emoji) == "⭐";
            let paid_allowed = is_paid_star && acts && self.rules().is_some_and(|rules| rules.paid);
            let can_big = self.rules().is_some_and(|rules| rules.big);
            // Joining one the conversation no longer takes, or past a limit, is not offered:
            // the chip says why instead.
            let refused = if is_paid_star {
                None
            } else {
                (!mine).then(|| self.refusal_kind(message, &toggled, is_burst)).flatten()
            };
            // Who reacted, where the network said or after an on-demand fetch; a chip that is
            // only a count keeps its own words.
            let named = !is_burst && !reaction.who.is_empty();
            let names = if named {
                let said = |id: &str| match self.person(id) {
                    Some(person) => self.who(&person, cx).0,
                    None => String::new(),
                };
                reactor_words(&reaction.who, said)
            } else {
                self.fetched_reactors
                    .get(&(message.id.clone(), toggled.clone(), is_burst))
                    .and_then(|(reactors, _)| {
                        let ids: Vec<String> = reactors.iter().map(|r| r.name.clone()).collect();
                        reactor_words(&ids, |name| name.to_owned())
                    })
            };
            let words = if is_paid_star {
                match &names {
                    Some(top) if paid_allowed => format!("Paid Star reaction · {top} · Click to send Stars"),
                    Some(top) => format!("Paid Star reaction · {top}"),
                    None if paid_allowed => "Paid Star reaction · Click to send Stars".to_owned(),
                    None => "Paid Star reaction".to_owned(),
                }
            } else {
                match (&refused, &names) {
                    (Some(why), _) => why.clone(),
                    (None, Some(names)) if is_burst => format!("Super reaction · {names}"),
                    (None, Some(names)) => names.clone(),
                    (None, None) if is_burst && mine => "Take back your super reaction".to_owned(),
                    (None, None) if is_burst => "Super react with this too".to_owned(),
                    (None, None) if mine => "Take back your reaction".to_owned(),
                    (None, None) if can_big => "React with this too (Shift+click for big)".to_owned(),
                    (None, None) => "React with this too".to_owned(),
                }
            };
            let joins = !is_paid_star && acts && refused.is_none();
            let clickable = joins || paid_allowed;
            let already_mine = reaction.mine;
            // Right-click names them in a list, where one was sent or the account can fetch them.
            // Left-click keeps its work: joining or taking the reaction back (or opening the
            // paid Star confirm sheet for ⭐).
            let can_reactors = self.open.as_ref().is_some_and(|open| open.can.reactors);
            let list = (!phone && (named || can_reactors)).then(|| {
                let open = message.clone();
                cx.listener(move |this: &mut Pane, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let at = event.position;
                    let open = open.clone();
                    this.open_reactors(open, ix, is_burst, at, window, cx);
                })
            });
            let built = if is_burst {
                let (base_chip, accent) = burst_chip(("chip", chip_ix).into(), mine, &reaction.burst_colors);
                let count = div()
                    .type_small()
                    .when(phone, |count| count.text_size(px(tokens_phone::scaled(14.))).line_height(px(tokens_phone::scaled(18.))))
                    .font_weight(theme::SEMIBOLD)
                    .text_color(if mine { accent } else { theme::text() })
                    .child(shown_count.to_string());
                base_chip
                    .child(div().type_micro().text_color(accent).child("✦"))
                    .child(emoji)
                    .child(count)
            } else {
                let count = div()
                    .type_small()
                    .when(phone, |count| count.text_size(px(tokens_phone::scaled(14.))).line_height(px(tokens_phone::scaled(18.))))
                    .font_weight(theme::MEDIUM)
                    .text_color(if mine { theme::accent_text() } else { theme::text_soft() })
                    .child(shown_count.to_string());
                chip(("chip", chip_ix).into(), mine)
                    .child(emoji)
                    .child(count)
            };
            row = row.child(
                built
                    .when((acts || is_paid_star) && !phone, |chip| chip.tooltip(tip(words, None)))
                    .when(paid_allowed, |chip| {
                        let id = id.clone();
                        chip.on_click(cx.listener(move |this, _, _, cx| this.open_paid_react(&id, already_mine, cx)))
                    })
                    .when(joins, |chip| {
                        chip.on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                            if is_burst {
                                this.react_kind(&id, &toggled, on, true, cx);
                            } else {
                                this.react_with(&id, &toggled, on, can_big && event.modifiers().shift, cx);
                            }
                        }))
                    })
                    .when_some(list, |chip, list| chip.on_mouse_down(MouseButton::Right, list))
                    .when(!clickable, |chip| chip.cursor_default()),
            );
        }
        let hidden = total_chips.saturating_sub(CHIPS);
        if hidden > 0 {
            let more = chip("chips-more".into(), false)
                .cursor_default()
                .type_small()
                .text_color(theme::text_soft())
                .when(!phone, |el| el.when_some(hidden_tip, |el, words| el.tooltip(tip(words, None))))
                .child(format!("+{hidden}"));
            row = row.child(more);
        }
        // Only lay out the hover-revealed add button when adding a reaction is actually allowed;
        // otherwise an invisible chip can wrap onto an empty ghost second line.
        let id = message.id.clone();
        let adds = acts && self.adds(message);
        if !adds {
            return Some(row.into_any_element());
        }
        let add = chip("chip-add".into(), false)
            .when(!phone, |chip| chip.opacity(0.))
            .when(!phone, |chip| chip.group_hover("row", |style| style.opacity(1.)))
            .child(icon(Icon::SmilePlus, if phone { 18. } else { 15. }, theme::text_soft()))
            .when(!phone, |chip| chip.tooltip(tip("Add reaction", None)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.open_picker(&id, event.position(), window, cx);
            }));
        Some(row.child(add).into_any_element())
    }

    fn reply_line(&mut self, reply: &protocol::ReplyTo, message_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics;
        let target = reply.id.clone();
        let gone = reply.deleted || (!reply.story && reply.sender.is_empty() && reply.snippet.is_empty());
        let label = if reply.story {
            if reply.sender.is_empty() || reply.sender.eq_ignore_ascii_case("You") {
                "Replied to your story".to_owned()
            } else {
                format!("Replied to {}'s story", reply.sender)
            }
        } else {
            reply.sender.clone()
        };
        let snippet = match (gone, reply.snippet.is_empty(), reply.story) {
            (true, _, _) => "Original message was deleted".to_owned(),
            (false, true, true) => String::new(),
            (false, true, false) => "Attachment".to_owned(),
            (false, false, _) => reply.snippet.replace('\n', " "),
        };
        let reply_key = format!("reply:{}", reply.id);
        let hide_reply = !gone
            && crate::gui::streamer::is_active()
            && crate::gui::streamer::contains_sensitive(&reply.snippet)
            && !self.revealed_sensitive.contains(&reply_key);
        let face = (!gone).then(|| {
            let (_, file) = reply.person.as_ref().map(|person| self.who(person, cx)).unwrap_or_default();
            let plate = match &reply.person {
                Some(person) => Avatar::new(person, &reply.sender, 16.),
                None => Avatar::named(&reply.sender, 16.),
            };
            self.face(plate, file.as_ref(), 16., Shape::Person, window, cx)
        });
        let thumb = (!gone).then(|| {
            let file = reply.thumb.as_ref()?;
            let edge = media::square_size(18., window.scale_factor());
            match self.still(file, edge, edge, Some(1.), cx) {
                Picture::Ready(image) => Some(
                    div()
                        .flex_none()
                        .size(px(18.))
                        .rounded(px(3.))
                        .overflow_hidden()
                        .child(anim::picture(image, ObjectFit::Cover, 3., Play::Hover)),
                ),
                _ => None,
            }
        }).flatten();
        // The elbow runs from the middle of the avatar column up and across to the quote.
        let reach = metrics.avatar / 2. + metrics.gap - 4.;
        div()
            .id("reply")
            .max_w(px(MEASURE + metrics.avatar + metrics.gap))
            .when(self.multi_select.active, |line| line.pl(px(18. + metrics.gap)))
            .when(!hide_reply, |line| line.h(px(20.)))
            .when(hide_reply, |line| line.min_h(px(20.)))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::text_faint())
            .when(!gone, |line| {
                line.cursor_pointer()
                    .hover(|style| style.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(&target, cx)))
            })
            .child(
                div().w(px(metrics.avatar + metrics.gap - 6.)).h_full().flex_none().relative().child(
                    div()
                        .absolute()
                        .left(px(metrics.avatar / 2. - 1.))
                        .top(px(9.))
                        .w(px(reach))
                        .h(px(14.))
                        .border_l_2()
                        .border_t_2()
                        .border_color(tokens::border_strong())
                        .rounded_tl(px(6.)),
                ),
            )
            .children(face)
            .children((!gone && !label.is_empty()).then(|| {
                div().flex_none().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child(label)
            }))
            .children(thumb)
            .children(if hide_reply {
                let key = reply_key;
                Some(
                    sensitive_cover(
                        SharedString::from(format!("reveal-reply-{message_id}-{}", reply.id)),
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.revealed_sensitive.insert(key.clone());
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                )
            } else {
                (!snippet.is_empty()).then(|| div().min_w_0().truncate().when(gone, |snippet| snippet.italic()).child(snippet).into_any_element())
            })
    }

    /// A channel post's footer: how many people saw it, as Telegram's apps say it. Telegram
    /// counts views for channel posts only, so a message without a count draws nothing.
    fn views_line(&self, message: &Message) -> Option<AnyElement> {
        let words = post_line(&message.signature, message.views, message.shares)?;
        Some(div().h(px(16.)).type_micro().text_color(theme::text_faint()).child(words).into_any_element())
    }

    fn thread_line(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A thread does not open inside a thread.
        let thread = message.thread.as_ref().filter(|_| !self.is_thread())?;
        let replies = if thread.replies == 1 { "1 reply".to_owned() } else { format!("{} replies", thread.replies) };
        let last = last_reply_label(now(), thread.last);
        // The latest repliers, overlapped, each ringed in the color behind them.
        // Placed by hand: each a ringed 24 pt disc, 19 pt on from the one before.
        let shown = thread.participants.len().min(protocol::THREAD_FACES);
        let mut faces = div().flex_none().relative().h(px(24.)).w(px(24. + 19. * shown.saturating_sub(1) as f32));
        for (ix, person) in thread.participants.iter().take(shown).enumerate() {
            let (name, file) = self.who(person, cx);
            let face = self.face(Avatar::new(person, &name, 20.), file.as_ref(), 20., Shape::Person, window, cx);
            faces = faces.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(19. * ix as f32))
                    .rounded_full()
                    .border_2()
                    .border_color(theme::canvas())
                    .group_hover("thread", |style| style.border_color(theme::raised()))
                    .child(face),
            );
        }
        let unread = (thread.unread > 0).then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(theme::accent_text())
                .child(div().size(px(6.)).rounded_full().bg(theme::accent()))
                .child(format!("{} new", thread.unread))
        });
        let root = message.clone();
        let line = div()
            .id("thread")
            .group("thread")
            .max_w(px(MEASURE))
            .h(px(30.))
            .mt(px(2.))
            .ml(px(-8.))
            .px(px(8.))
            .rounded(px(theme::R_CARD))
            .border_1()
            .border_color(gpui::transparent_black())
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised()).border_color(theme::hairline()))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PaneEvent::OpenThread(root.clone()))))
            .when(thread.participants.is_empty(), |line| line.child(icon(Icon::Thread, 14., theme::accent_text())))
            .when(!thread.participants.is_empty(), |line| line.child(faces))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::accent_text()).child(replies))
            .children(last.map(|last| div().type_small().text_color(theme::text_faint()).child(last)))
            .children(unread)
            .child(
                div()
                    .flex_none()
                    .opacity(0.)
                    .group_hover("thread", |style| style.opacity(1.))
                    .child(icon(Icon::ChevronRight, 13., theme::text_faint())),
            );
        Some(div().flex().child(line).into_any_element())
    }

    /// Under one of my messages still going: "Sending…", or for files the bar of what has
    /// gone, moved only by chatd's `Upload` changes, and Cancel.
    fn sending(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.state != SendState::Pending {
            return None;
        }
        if message.waiting {
            let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
            return self.waiting_mark(client_id, cx);
        }
        let client_id = message.client_id.clone()?;
        let words = div().type_small().text_color(theme::text_faint()).font_family(theme::FONT);
        let Some(&(sent, total)) = self.uploads.get(&client_id) else {
            return Some(div().h(px(18.)).flex().items_center().child(words.child("Sending…")).into_any_element());
        };
        let conversation = self.key()?.clone();
        let done = if total == 0 { 0. } else { (sent as f32 / total as f32).clamp(0., 1.) };
        let bar = div()
            .flex_none()
            .w(px(240.))
            .h(px(4.))
            .rounded(px(2.))
            .bg(theme::border())
            .child(div().h_full().w(px(240. * done)).rounded(px(2.)).bg(theme::accent()));
        let cancel = div()
            .id("cancel-upload")
            .flex_none()
            .size(px(20.))
            .rounded(px(theme::R_CHIP))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .tooltip(tip("Cancel upload", None))
            .child(icon(Icon::Close, 12., theme::text_soft()))
            .on_click(cx.listener(move |_, _, _, cx| {
                let request = protocol::Request::CancelSend { conversation: conversation.clone(), client_id: client_id.clone() };
                cx.background_executor().spawn(async move { crate::client::ask(&request) }).detach();
            }));
        let sizes: Vec<u64> = message.attachments.iter().map(|attachment| attachment.file.bytes).collect();
        let line = div()
            .h(px(20.))
            .mt(px(4.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(bar)
            .child(words.child(uploading_words(&sizes, sent, total)))
            .child(cancel);
        Some(line.into_any_element())
    }

    /// Under one of my messages written while the account was offline: a clock, who it is
    /// waiting for, and Cancel. Not "Sending…", which is a send the adapter already has.
    fn waiting_mark(&self, client_id: String, cx: &mut Context<Self>) -> Option<AnyElement> {
        let conversation = self.key()?.clone();
        let network = conversation.network.label();
        let words = div().type_small().text_color(theme::text_faint()).font_family(theme::FONT);
        let cancel = div()
            .id("cancel-wait")
            .flex_none()
            .size(px(20.))
            .rounded(px(theme::R_CHIP))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .tooltip(tip("Cancel", None))
            .child(icon(Icon::Close, 12., theme::text_soft()))
            .on_click(cx.listener(move |_, _, _, cx| {
                let request = protocol::Request::CancelSend { conversation: conversation.clone(), client_id: client_id.clone() };
                cx.background_executor().spawn(async move { crate::client::ask(&request) }).detach();
            }));
        let line = div()
            .h(px(20.))
            .mt(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(glyph("icons/clock.svg", 12., theme::text_faint()))
            .child(words.child(waiting_line(network)))
            .child(cancel);
        Some(line.into_any_element())
    }

    /// What a message that did not go says under its body: the network's reason, then the two
    /// things that can be done about it, as words.
    fn unsent(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let SendState::Failed { reason } = &message.state else {
            return None;
        };
        let key = self.key()?.clone();
        let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
        let (again, drop) = ((key.clone(), client_id.clone()), (key, client_id));
        let actions = div().min_w_0().max_w(px(MEASURE)).flex().flex_wrap().items_center().gap(px(10.))
            .child(
                div()
                    .id("retry")
                    .flex_none()
                    .ml(px(19.))
                    .font_weight(theme::SEMIBOLD)
                    .text_color(tokens::link())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("Retry")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (conversation, client_id) = again.clone();
                        let _ = this.shell.update(cx, |shell, cx| shell.retry_send(conversation, client_id, cx));
                    })),
            )
            .child(
                div()
                    .id("discard")
                    .flex_none()
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (conversation, client_id) = drop.clone();
                        let _ = this.shell.update(cx, |shell, cx| shell.discard_send(conversation, client_id, cx));
                    })),
            );
        let line = div().w_full().min_w_0().max_w(px(MEASURE)).mt(px(3.)).flex().flex_col().gap(px(4.)).type_small()
            .child(
                div().min_w_0().max_w_full().flex().items_start().gap(px(6.)).text_color(theme::danger())
                    .child(div().pt(px(2.)).child(icon(Icon::Alert, 13., theme::danger())))
                    .child(div().flex_1().min_w_0().max_w_full().whitespace_normal().child(format!("Not sent · {reason}"))),
            )
            .child(actions);
        Some(line.into_any_element())
    }

    /// The tinted foot of an only-you reply. Dismiss removes it here, and on Discord when that is allowed.
    fn ephemeral_note(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !message.ephemeral || message.deleted {
            return None;
        }
        let conversation = self.key()?.clone();
        let message_id = message.id.clone();
        let dismiss = div()
            .id(name_id("dismiss", &message.id))
            .flex_none()
            .cursor_pointer()
            .text_color(tokens::link())
            .hover(|style| style.underline())
            .child("Dismiss")
            .on_click(cx.listener(move |_, _, _, cx| {
                let request = Request::Delete {
                    conversation: conversation.clone(),
                    message_id: message_id.clone(),
                    scope: protocol::DeleteScope::Everyone,
                };
                cx.background_executor().spawn(async move { crate::client::ask(&request) }).detach();
            }));
        Some(
            div()
                .mt(px(4.))
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(theme::text_faint())
                .child("Only you can see this ·")
                .child(dismiss)
                .into_any_element(),
        )
    }
}

/// Bytes as the upload line says them: "3.1 MB", "820 KB".
/// "Uploading 2 of 3 · 3.1 of 5.4 MB" while a message's files go, "Uploading · …" for one.
fn uploading_words(sizes: &[u64], sent: u64, total: u64) -> String {
    let amount = format!("{} of {}", megabytes(sent), megabytes(total));
    if sizes.len() < 2 {
        return format!("Uploading · {amount}");
    }
    // The file under way: the first whose end the bytes sent have not reached.
    let mut end = 0;
    let at = sizes.iter().position(|bytes| {
        end += bytes;
        sent < end
    });
    format!("Uploading {} of {} · {amount}", at.map_or(sizes.len(), |at| at + 1), sizes.len())
}

/// The row's receipt, if this message is my newest and the newest of all, sent and not gone.
fn receipt_for(message: &Message, window: &super::pages::Window, receipt: Option<Receipt>) -> Option<Receipt> {
    let newest = window.messages.last().filter(|_| !window.newer)?;
    let mine = message.outgoing && message.state == SendState::Sent && !message.deleted && newest.id == message.id;
    receipt.filter(|receipt| mine && *receipt != Receipt::Sending)
}

/// The 12 px glyph on my newest message (`design/06-live-signals.md` §4): one check once sent,
/// on every network; two once the other side's device has it; two in `accent` once read.
fn receipt_mark(id: &str, receipt: Receipt) -> AnyElement {
    let (icon, color, words) = match receipt {
        Receipt::Read => ("icons/check-check.svg", theme::accent(), "Read"),
        Receipt::Delivered => ("icons/check-check.svg", theme::text_faint(), "Delivered"),
        Receipt::Sent | Receipt::Sending => ("icons/check.svg", theme::text_faint(), "Sent"),
    };
    div()
        .id(name_id("receipt", id))
        .flex_none()
        .flex()
        .items_center()
        .tooltip(tip(words, None))
        .child(glyph(icon, 12., color))
        .into_any_element()
}

/// The bell-off after a message's time when it went without notification.
fn silent_mark(message: &Message) -> Option<AnyElement> {
    let from_send = message.client_id.as_deref().is_some_and(crate::gui::composer::sent_silently);
    if !message.silent && !from_send {
        return None;
    }
    let id = message.client_id.as_deref().unwrap_or(message.id.as_str());
    Some(
        div()
            .id(name_id("silent", id))
            .flex_none()
            .flex()
            .items_center()
            .tooltip(tip("Sent without notification", None))
            .child(glyph("icons/bell-off.svg", 12., theme::text_faint()))
            .into_any_element(),
    )
}

/// "<person> used /command" above a bot's answer, aligned with the message column.
fn command_used(via: &Via, metrics: Metrics, selecting: bool) -> Div {
    let command = via.command.trim_start_matches('/');
    let indent = metrics.avatar + metrics.gap + if selecting { 18. + metrics.gap } else { 0. };
    div()
        .h(px(18.))
        .pl(px(indent))
        .mb(px(2.))
        .flex()
        .items_center()
        .type_small()
        .text_color(theme::text_faint())
        .child(format!("{} used /{command}", via.who))
}

fn megabytes(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1000)),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.),
    }
}

/// The size the network gave for an attachment's picture: the file's, or its still's.
fn natural(attachment: &Attachment) -> (u32, u32) {
    let still = attachment.thumb.as_ref().unwrap_or(&attachment.file);
    if attachment.file.width > 0 && attachment.file.height > 0 {
        (attachment.file.width, attachment.file.height)
    } else {
        (still.width, still.height)
    }
}

/// The message that says most of who someone is here: their newest with a picture, else their
/// newest. A tombstone keeps the name and loses the picture, so one that has it is looked for first.
fn said_by<'a>(root: Option<&'a Rc<Message>>, messages: &'a [Rc<Message>], id: &str) -> Option<&'a Message> {
    let theirs = || root.into_iter().chain(messages).rev().filter(|message| message.sender.id == id);
    let found = theirs().find(|message| message.sender.avatar.is_some()).or_else(|| theirs().next());
    found.map(|message| &**message)
}

/// The picture on a message of mine: the one it names, else the account's, which follows a
/// change of picture. One I am sending is not from the network yet and names none. Without the
/// account's (a network that keeps no row of me), my newest message here lends the picture it
/// was stored with, which may since have changed.
fn own_face(
    message: &Message,
    root: Option<&Rc<Message>>,
    messages: &[Rc<Message>],
    me: &str,
    account: impl FnOnce() -> Option<MediaFile>,
) -> Option<MediaFile> {
    let shown = || match me {
        // Without my id nothing here is known to be mine.
        "" => None,
        me => said_by(root, messages, me)?.sender.avatar.clone(),
    };
    message.sender.avatar.clone().or_else(account).or_else(shown)
}

/// Some networks supply a reply count before they supply a timestamp for the last reply.
/// Uses `time::stamp` ("Last reply 9:24 PM") so it never goes stale between redraws.
fn last_reply_label(now: i64, last: i64) -> Option<String> {
    (last > 0).then(|| format!("Last reply {}", time::stamp(now, last)))
}

/// A view count the way Telegram's apps say it: "987", "12.4K", "1.2M". The number is cut,
/// not rounded, so a count never reads as more than Telegram said.
/// A channel post's footer: who signed it, how many saw it, how many times it was forwarded.
/// Each part is said only when the network gave it; a share count of none is not said.
fn post_line(signature: &str, views: Option<u32>, shares: Option<u32>) -> Option<String> {
    let parts: Vec<String> = [
        (!signature.is_empty()).then(|| signature.to_owned()),
        views.map(|views| format!("{} views", views_short(views))),
        shares.filter(|shares| *shares > 0).map(|shares| format!("{} {}", views_short(shares), if shares == 1 { "share" } else { "shares" })),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn views_short(views: u32) -> String {
    let (divisor, suffix) = match views {
        0..=999 => return views.to_string(),
        1_000..=999_999 => (1_000, "K"),
        _ => (1_000_000, "M"),
    };
    let tenths = views / (divisor / 10);
    match tenths % 10 {
        0 => format!("{}{suffix}", tenths / 10),
        part => format!("{}.{part}{suffix}", tenths / 10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn picture(id: &str) -> MediaFile {
        MediaFile::new(id, MediaKind::Avatar)
    }

    /// One of mine the network has, with the picture it was stored with.
    fn mine(id: &str, avatar: Option<&str>) -> Rc<Message> {
        let mut message = Message::plain(id, "Jo", "said", 10, true);
        message.sender.id = "me".into();
        message.sender.avatar = avatar.map(picture);
        Rc::new(message)
    }

    #[test]
    fn a_queued_send_is_waiting_for_its_network_not_on_its_way() {
        assert_eq!(waiting_line("Discord"), "Waiting for Discord");
        assert!(!waiting_line("Discord").contains("Sending"));
    }

    /// One of mine still going: chatd names no sender on it.
    fn sending() -> Message {
        let mut message = Message::plain("c1", "You", "going", 20, true);
        message.state = SendState::Pending;
        message
    }

    fn face(message: &Message, messages: &[Rc<Message>], account: Option<&str>) -> Option<String> {
        own_face(message, None, messages, "me", || account.map(picture)).map(|file| file.id)
    }

    #[test]
    fn a_post_says_its_signature_views_and_shares_and_nothing_it_was_not_given() {
        assert_eq!(post_line("", None, None), None);
        assert_eq!(post_line("", Some(12_400), None).as_deref(), Some("12.4K views"));
        assert_eq!(post_line("Mira K.", Some(12_400), Some(31)).as_deref(), Some("Mira K. · 12.4K views · 31 shares"));
        assert_eq!(post_line("", Some(7), Some(1)).as_deref(), Some("7 views · 1 share"));
        assert_eq!(post_line("", Some(7), Some(0)).as_deref(), Some("7 views"));
        // An anonymous admin's title in a group, where nothing is counted.
        assert_eq!(post_line("Trail boss", None, None).as_deref(), Some("Trail boss"));
    }

    #[test]
    fn a_view_count_is_said_the_way_telegrams_apps_say_it() {
        assert_eq!(views_short(999), "999");
        assert_eq!(views_short(1_000), "1K");
        assert_eq!(views_short(1_024), "1K");
        assert_eq!(views_short(12_400), "12.4K");
        assert_eq!(views_short(999_999), "999.9K");
        assert_eq!(views_short(1_000_000), "1M");
        assert_eq!(views_short(1_200_000), "1.2M");
    }

    #[test]
    fn the_last_reply_label_requires_a_known_timestamp() {
        let now = 1_794_553_200;
        assert_eq!(last_reply_label(now, 0), None);
        let recent = last_reply_label(now, now - 20).expect("known timestamp produces a label");
        assert!(recent.starts_with("Last reply "));
        assert!(!recent.contains("ago"));
    }

    #[test]
    fn only_my_newest_message_carries_the_mark_and_only_once_sent() {
        let theirs = Rc::new(Message::plain("3", "mara", "hi", 30, false));
        let window = |messages: Vec<Rc<Message>>, newer| super::super::pages::Window { messages, older: false, newer };
        let all = window(vec![mine("1", None), mine("2", None)], false);
        assert_eq!(receipt_for(&all.messages[1], &all, Some(Receipt::Read)), Some(Receipt::Read));
        // Older ones are implied; the conversation says nothing of them.
        assert_eq!(receipt_for(&all.messages[0], &all, Some(Receipt::Read)), None);
        // Someone spoke after me, or newer messages are not loaded: nothing to mark here.
        let answered = window(vec![mine("1", None), theirs.clone()], false);
        assert_eq!(receipt_for(&answered.messages[0], &answered, Some(Receipt::Sent)), None);
        let partial = window(vec![mine("1", None)], true);
        assert_eq!(receipt_for(&partial.messages[0], &partial, Some(Receipt::Sent)), None);
        // One still going says "Sending…" instead.
        let going = window(vec![Rc::new(sending())], false);
        assert_eq!(receipt_for(&going.messages[0], &going, Some(Receipt::Sent)), None);
        assert_eq!(receipt_for(&all.messages[1], &all, Some(Receipt::Sending)), None);
    }

    #[test]
    fn my_message_shows_its_own_picture_then_the_accounts_then_my_newest_here() {
        let messages = [mine("1", Some("shown-here"))];
        assert_eq!(face(&mine("2", Some("its-own")), &messages, Some("account")).as_deref(), Some("its-own"));
        // One being sent names none.
        assert_eq!(face(&sending(), &messages, Some("account")).as_deref(), Some("account"));
        assert_eq!(face(&sending(), &messages, None).as_deref(), Some("shown-here"));
        assert_eq!(face(&sending(), &[], None), None);
    }

    #[test]
    fn a_picture_i_have_since_changed_does_not_come_back_from_my_earlier_messages() {
        let messages = [mine("1", Some("before")), mine("2", Some("before"))];
        assert_eq!(face(&sending(), &messages, Some("after")).as_deref(), Some("after"));
    }

    #[test]
    fn only_my_newest_picture_here_is_lent_and_only_once_my_id_is_known() {
        // A newer one of mine without a picture, a tombstone, does not lose it.
        let messages = [mine("1", Some("older")), mine("2", Some("newer")), mine("3", None)];
        assert_eq!(face(&sending(), &messages, None).as_deref(), Some("newer"));
        let mut theirs = Message::plain("4", "mara", "hi", 30, false);
        theirs.sender.id = "mara".into();
        theirs.sender.avatar = Some(picture("mara"));
        assert_eq!(face(&sending(), &[Rc::new(theirs)], None), None);
        assert_eq!(own_face(&sending(), None, &messages, "", || None), None);
    }

    #[test]
    fn call_line_spec_formats_each_outcome_and_direction() {
        let make = |outcome, outgoing, seconds| {
            let mut m = Message::plain("r1", "Bea", "fallback", 100, outgoing);
            m.kind = MessageKind::System;
            m.call = Some(protocol::CallNote { outcome, caller_id: "u1".into(), seconds, participants: Vec::new(), video: false, call_id: None });
            m
        };
        let missed_in = call_line_spec(&make(protocol::CallOutcome::Missed, false, None), "Bea");
        assert!(missed_in.missed);
        assert_eq!(missed_in.label, "Missed call from Bea");
        assert_eq!(missed_in.pill, Some("Call back"));

        let missed_out = call_line_spec(&make(protocol::CallOutcome::Missed, true, None), "Bea");
        assert!(!missed_out.missed);
        assert_eq!(missed_out.label, "You called Bea · no answer");
        assert_eq!(missed_out.pill, Some("Call again"));

        let dec_in = call_line_spec(&make(protocol::CallOutcome::Declined, false, None), "Bea");
        assert_eq!(dec_in.label, "You declined a call from Bea");
        assert_eq!(dec_in.pill, None);

        let dec_out = call_line_spec(&make(protocol::CallOutcome::Declined, true, None), "Bea");
        assert_eq!(dec_out.label, "Bea declined the call");
        assert_eq!(dec_out.pill, Some("Call again"));

        let can_in = call_line_spec(&make(protocol::CallOutcome::Cancelled, false, None), "Bea");
        assert_eq!(can_in.label, "Bea cancelled the call");
        let can_out = call_line_spec(&make(protocol::CallOutcome::Cancelled, true, None), "Bea");
        assert_eq!(can_out.label, "You cancelled the call");

        let ended_dur = call_line_spec(&make(protocol::CallOutcome::Ended, false, Some(720)), "Bea");
        assert_eq!(ended_dur.label, "Call with Bea · 12 min");
        let ended_plain = call_line_spec(&make(protocol::CallOutcome::Ended, true, None), "Bea");
        assert_eq!(ended_plain.label, "Call with Bea");
        assert_eq!(ended_plain.pill, Some("Call back"));

        let busy = call_line_spec(&make(protocol::CallOutcome::Busy, true, None), "Bea");
        assert_eq!(busy.label, "Bea's line was busy");
        assert_eq!(busy.pill, Some("Call again"));

        let mut video_missed = make(protocol::CallOutcome::Missed, false, None);
        video_missed.call.as_mut().unwrap().video = true;
        let vm = call_line_spec(&video_missed, "Bea");
        assert!(vm.missed);
        assert_eq!(vm.label, "Missed video call from Bea");
        assert_eq!(vm.pill, Some("Call back"));
    }

    #[test]
    fn expiry_label_formats_countdown_cleanly() {
        assert_eq!(expiry_label(100, 100), Some("expiring".into()));
        assert_eq!(expiry_label(125, 100), Some("25s".into()));
        assert_eq!(expiry_label(400, 100), Some("5m".into()));
        assert_eq!(expiry_label(7_300, 100), Some("2h".into()));
        assert_eq!(expiry_label(172_900, 100), Some("2d".into()));
    }

    #[test]
    fn expiry_tooltip_avoids_expiring_grammar_bug_and_inline_expiry_hides_chat_default() {
        assert_eq!(expiry_tooltip(100, 100), "Expiring now");
        assert_eq!(expiry_tooltip(90, 100), "Expiring now");
        let tip = expiry_tooltip(100 + 7_200, 100);
        assert!(tip.starts_with("Disappears in 2h ("), "got {tip}");
        // In a 7-day disappearing chat, a message with 6d left does not spam an inline badge.
        assert!(!should_show_inline_expiry(100 + 6 * 86_400, 100, Some(7 * 86_400)));
        // Urgent (< 1 hour left) still shows inline.
        assert!(should_show_inline_expiry(100 + 1_800, 100, Some(7 * 86_400)));
        // Standalone self-destruct message in a non-disappearing chat still shows inline.
        assert!(should_show_inline_expiry(100 + 6 * 86_400, 100, None));
        assert!(should_show_inline_expiry(100 + 6 * 86_400, 100, Some(0)));
    }

    #[test]
    fn maps_url_extracted_from_location_card() {
        let card = Card {
            title: "Ferry Building".into(),
            text: "1 Ferry Building\ngeo:37.7749,-122.4194".into(),
            fields: vec![
                ("Latitude".into(), "37.7749".into()),
                ("Longitude".into(), "-122.4194".into()),
            ],
            footer: "Open in Maps".into(),
            color: None,
            sponsored: false,
            ..Card::default()
        };
        assert_eq!(
            maps_url_for_card(&card).as_deref(),
            Some("https://maps.google.com/?q=37.7749,-122.4194")
        );
    }
}

/// Extracts a Google Maps URL from a location card's `geo:` line or Latitude/Longitude fields.
fn maps_url_for_card(card: &Card) -> Option<String> {
    for line in card.text.lines() {
        if let Some(rest) = line.trim().strip_prefix("geo:") {
            let coords = rest.split([';', '?', ' ']).next().unwrap_or("").trim();
            if coords.contains(',') {
                return Some(format!("https://maps.google.com/?q={coords}"));
            }
        }
    }
    let lat = card.fields.iter().find(|(k, _)| k == "Latitude").map(|(_, v)| v.trim());
    let lng = card.fields.iter().find(|(k, _)| k == "Longitude").map(|(_, v)| v.trim());
    if let (Some(lat), Some(lng)) = (lat, lng) {
        if !lat.is_empty() && !lng.is_empty() {
            return Some(format!("https://maps.google.com/?q={lat},{lng}"));
        }
    }
    None
}

/// Reads the first `TEL` phone number from a downloaded `.vcf` file on disk.
fn first_vcard_phone(path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((key, val)) = line.split_once(':') {
            let prop = key.split(';').next().unwrap_or("").trim();
            if prop.eq_ignore_ascii_case("TEL") {
                let clean = val.trim().strip_prefix("tel:").unwrap_or(val.trim()).trim();
                if !clean.is_empty() {
                    return Some(clean.to_owned());
                }
            }
        }
    }
    None
}

/// Short remaining-time label for a disappearing message's `expires_at` timestamp.
pub(crate) fn expiry_label(expires_at: i64, now: i64) -> Option<String> {
    let left = expires_at - now;
    if left <= 0 {
        return Some("expiring".to_owned());
    }
    if left >= 86_400 {
        Some(format!("{}d", left / 86_400))
    } else if left >= 3_600 {
        Some(format!("{}h", left / 3_600))
    } else if left >= 60 {
        Some(format!("{}m", left / 60))
    } else {
        Some(format!("{left}s"))
    }
}

/// Human-readable expiration detail for a message tooltip.
pub(crate) fn expiry_tooltip(expires_at: i64, now: i64) -> String {
    let left = expires_at - now;
    if left <= 0 {
        return "Expiring now".to_owned();
    }
    let label = expiry_label(expires_at, now).unwrap_or_default();
    format!("Disappears in {label} ({})", time::tooltip(expires_at))
}

/// Full tooltip for a message timestamp, appending disappearing expiry when set.
pub(crate) fn message_timestamp_tooltip(message: &Message, now: i64) -> String {
    let sent = time::tooltip(message.sent);
    match message.expires_at {
        Some(expires_at) => format!("{sent} · {}", expiry_tooltip(expires_at, now)),
        None => sent,
    }
}

/// Only show an inline expiry badge in the message header when the message is disappearing soon
/// (`< 1h`) or has a standalone self-destruct timer in a chat with no chat-level timer. Normal
/// messages in a chat with a header disappearing timer keep their rows clean and show expiry on
/// timestamp hover.
pub(crate) fn should_show_inline_expiry(expires_at: i64, now: i64, chat_disappearing: Option<u32>) -> bool {
    let left = expires_at - now;
    left < 3_600 || chat_disappearing.unwrap_or(0) == 0
}
