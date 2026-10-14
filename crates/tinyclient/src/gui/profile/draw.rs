//! The parts the pane, the card and Details share, drawn once: the head, the tag after a name,
//! fact rows, role chips, the heads-up, the switch.

use super::facts::{self, HeadsUpWords, PresenceLine, Role, Tone};
use crate::gui::components::{glyph, Avatar, Shape as Plate};
use crate::gui::people::presence::{self, Shape};
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, rgb, Div, ElementId, Rgba, SharedString, Stateful};
use protocol::{Mark, Network, Person, Presence};

/// The band behind the head, when the network gives a colour.
pub const BAND: f32 = 48.;
pub const AVATAR: f32 = 64.;

/// `band(c)`: the colour at 40% over the surface.
pub fn band(color: u32, surface: Rgba) -> Rgba {
    let mut tinted = rgb(color);
    tinted.a = 0.4;
    theme::over(tinted, surface)
}

/// A tag after a name: Bot, App, Guest, External, Private; Scam and Fake in danger.
pub fn tag(words: impl Into<SharedString>, danger: bool) -> Div {
    let (fill, text) = if danger { (theme::danger_soft(), theme::danger()) } else { (theme::hover(), theme::text_soft()) };
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .rounded(px(4.))
        .bg(fill)
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::MEDIUM)
        .text_color(text)
        .child(words.into())
}

/// The network's own word for an account that is not a person: Telegram says bot, the others
/// app (Discord's bots are apps there too).
pub fn bot_word(network: Network) -> &'static str {
    match network {
        Network::Telegram => "Bot",
        _ => "App",
    }
}

/// The one mark after a person's name: a tag, or Verified's check.
pub fn mark_of(person: &Person) -> Option<Div> {
    if person.bot {
        return Some(tag(bot_word(person.key.network), false));
    }
    // CONTRACT: `badge-check` is not in the asset list; the plain check stands in.
    Some(match person.mark? {
        Mark::Verified => glyph("icons/check.svg", 14., theme::text_soft()).into_any_element(),
        Mark::Scam => tag("Scam", true).into_any_element(),
        Mark::Fake => tag("Fake", true).into_any_element(),
        Mark::Guest => tag("Guest", false).into_any_element(),
        Mark::External => tag("External", false).into_any_element(),
        Mark::Deactivated => tag("Deactivated", false).into_any_element(),
    })
    .map(|mark| div().flex_none().flex().items_center().child(mark))
}

/// A lone "…" at the head's right, level with the foot of the picture, so it never sits by
/// itself at the left of an empty action row.
pub fn head_more(more: gpui::AnyElement) -> Div {
    div().absolute().top(px(16. + AVATAR - 28.)).right(px(theme::PANE_PAD)).child(more)
}

/// An action under a head: it shares the row and gives way, its words cut before they spill.
pub fn action(id: impl Into<ElementId>, kind: crate::gui::components::ButtonKind, icon: &'static str, words: String, out: bool) -> Stateful<Div> {
    let color = match kind {
        crate::gui::components::ButtonKind::Primary => theme::on_accent(),
        _ => theme::text_soft(),
    };
    crate::gui::components::button(id, kind, "")
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .pl(px(10.))
        .child(glyph(icon, 16., color))
        .child(div().min_w_0().truncate().child(words))
        .children(out.then(|| glyph("icons/external-link.svg", 14., theme::text_faint())))
}

/// Name and mark, one line, ellipsis.
pub fn name_line(name: impl Into<SharedString>, mark: Option<Div>) -> Div {
    // The tag wraps under a long name rather than cut it.
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(6.))
        .min_w_0()
        .child(div().flex_shrink_0().max_w_full().truncate().type_title().child(name.into()))
        .children(mark)
}

/// Line 4: handle and pronouns, or a title. Collapses when empty.
pub fn soft_line(words: String) -> Option<Div> {
    (!words.is_empty()).then(|| div().truncate().type_ui().text_color(theme::text_soft()).child(words))
}

fn shape_of(presence: Presence) -> Option<Shape> {
    Shape::of(presence)
}

/// The presence glyph, the word, what they say and until when. `lines` caps the wrap.
pub fn presence_row(line: &PresenceLine, surface: Rgba, lines: usize) -> Option<Div> {
    if line.is_empty() {
        return None;
    }
    let glyph = line.glyph.and_then(shape_of).map(|shape| div().flex_none().pt(px(4.)).child(presence::inline(shape, 10., surface)));
    let said = match (line.word.is_empty(), line.status.is_empty()) {
        (_, true) => String::new(),
        (true, false) => line.status.clone(),
        (false, false) => format!(" · {}", line.status),
    };
    let until = if line.until.is_empty() { String::new() } else { format!(" · {}", line.until) };
    // One run of text so it wraps as a sentence.
    let text = gpui::StyledText::new(format!("{}{said}{until}", line.word)).with_highlights([
        (0..line.word.len(), gpui::HighlightStyle { color: Some(theme::text().into()), ..Default::default() }),
        (line.word.len()..line.word.len() + said.len(), gpui::HighlightStyle { color: Some(theme::text_soft().into()), ..Default::default() }),
        (
            line.word.len() + said.len()..line.word.len() + said.len() + until.len(),
            gpui::HighlightStyle { color: Some(theme::text_faint().into()), ..Default::default() },
        ),
    ]);
    Some(
        div()
            .flex()
            .items_start()
            .gap(px(6.))
            .type_ui()
            .children(glyph)
            .child(div().min_w_0().line_clamp(lines).child(text)),
    )
}

/// A section's label: 11/500 `text_faint`, 28 high with the label at its foot (24 when first).
pub fn label(words: &'static str, first: bool) -> Div {
    div()
        .h(px(if first { theme::SECTION_FIRST } else { theme::SECTION }))
        .pb(px(6.))
        .flex()
        .items_end()
        .type_micro()
        .text_color(theme::text_faint())
        .child(words)
}

/// A fact row: glyph 16 at 0, words at 24, softer words after; wraps.
pub fn fact(id: impl Into<ElementId>, icon: &'static str, words: impl Into<SharedString>, soft: impl Into<SharedString>) -> Stateful<Div> {
    let soft: SharedString = soft.into();
    div()
        .id(id)
        .min_h(px(theme::ROW_ONE))
        .mx(px(-8.))
        .px(px(8.))
        .py(px(5.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_start()
        .gap(px(8.))
        .type_ui()
        .child(div().flex_none().pt(px(1.)).child(glyph(icon, 16., theme::text_soft())))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(words.into())
                .children((!soft.is_empty()).then(|| div().text_color(theme::text_soft()).child(soft))),
        )
}

/// A fact whose words and softer words run on one line, as "In voice · #trail · Moss & Lichen".
pub fn fact_inline(id: impl Into<ElementId>, icon: &'static str, words: String, soft: String) -> Stateful<Div> {
    let text = gpui::StyledText::new(format!("{words}{soft}")).with_highlights([(
        words.len()..words.len() + soft.len(),
        gpui::HighlightStyle { color: Some(theme::text_soft().into()), ..Default::default() },
    )]);
    div()
        .id(id)
        .min_h(px(theme::ROW_ONE))
        .mx(px(-8.))
        .px(px(8.))
        .py(px(5.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_start()
        .gap(px(8.))
        .type_ui()
        .child(div().flex_none().pt(px(1.)).child(glyph(icon, 16., theme::text_soft())))
        .child(div().flex_1().min_w_0().child(text))
}

/// A Space as a place row: 20 rounded square, name 13.
pub fn place_row(id: impl Into<ElementId>, space: &protocol::Key, name: &str, icon: Option<&protocol::MediaFile>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(theme::ROW_ONE))
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .gap(px(8.))
        .type_ui()
        .child(Avatar::new(space, name, 20.).shape(Plate::Place).image(icon).build())
        .child(div().min_w_0().truncate().child(name.to_owned()))
}

pub fn role_chip(words: String) -> Div {
    // CONTRACT: `Profile.roles` carries names only, so the chip's 8 dot in the role's colour
    // is not drawn.
    div()
        .flex_none()
        .h(px(20.))
        .px(px(6.))
        .rounded(px(4.))
        .bg(theme::hover())
        .flex()
        .items_center()
        .type_small()
        .text_color(theme::text_soft())
        .child(words)
}

/// Role: title and department, or the nickname in a Space and its roles.
pub fn role(role: &Role, place_icon: Option<Div>, chips_cap: Option<usize>) -> Div {
    match role {
        Role::Work { title, department } => div()
            .flex()
            .flex_col()
            .type_ui()
            .children((!title.is_empty()).then(|| div().child(title.clone())))
            .children((!department.is_empty()).then(|| div().text_color(theme::text_soft()).child(department.clone()))),
        Role::Place { nickname, place, roles, more } => {
            let cap = chips_cap.unwrap_or(roles.len()).min(roles.len());
            let hidden = more + (roles.len() - cap);
            let named = gpui::StyledText::new(if nickname.is_empty() { format!("In {place}") } else { format!("{nickname} in {place}") })
                .with_highlights((!nickname.is_empty()).then(|| {
                    (0..nickname.len(), gpui::HighlightStyle { font_weight: Some(theme::SEMIBOLD), color: Some(theme::text().into()), ..Default::default() })
                }));
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .type_ui()
                        .text_color(theme::text_soft())
                        .children(place_icon)
                        .child(div().min_w_0().truncate().child(named)),
                )
                .when(!roles.is_empty(), |column| {
                    column.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .children(roles[..cap].iter().cloned().map(role_chip))
                            .children((hidden > 0).then(|| role_chip(format!("+{hidden}")))),
                    )
                })
        }
    }
}

pub fn tone_color(tone: Tone) -> Rgba {
    match tone {
        Tone::Danger => theme::danger(),
        Tone::Warning => theme::warning(),
        Tone::Soft => theme::text_soft(),
    }
}

/// The pane's heads-up: a banner, `raised` and a hairline, glyph in its colour.
pub fn heads_up_banner(words: &HeadsUpWords, action: Option<gpui::AnyElement>) -> Div {
    div()
        .min_h(px(theme::BANNER))
        .px(px(10.))
        .py(px(7.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::hairline())
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().flex_none().pt(px(1.)).child(glyph(words.glyph, 16., tone_color(words.tone))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().type_ui().child(words.line.clone()))
                .children(words.note.clone().map(|note| div().type_small().text_color(theme::text_soft()).child(note))),
        )
        .children(action)
}

/// The card's heads-up: one 12/16 line with a 14 glyph, no banner.
pub fn heads_up_line(words: &HeadsUpWords) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(6.))
        .type_small()
        .text_color(theme::text_soft())
        .child(div().flex_none().pt(px(1.)).child(glyph(words.glyph, 14., tone_color(words.tone))))
        .child(div().min_w_0().line_clamp(2).child(words.line.clone()))
}

/// An on/off switch, 32 × 18. On is accent.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(32.))
        .h(px(18.))
        .p(px(2.))
        .rounded(px(9.))
        .cursor_pointer()
        .bg(if on { theme::accent() } else { theme::selected() })
        .flex()
        .when(on, |track| track.justify_end())
        .child(div().size(px(14.)).rounded(px(7.)).bg(theme::on_accent()))
}

/// "Notifications" left, the control right, 28 high.
pub fn notifications_row(control: impl IntoElement) -> Div {
    div()
        .h(px(theme::ROW_ONE))
        .flex()
        .items_center()
        .justify_between()
        .type_ui()
        .child("Notifications")
        .child(control)
}

/// Static placeholders in the commonest geometry, shown only after 150 ms of waiting.
pub fn skeleton() -> Div {
    let bar = |fraction: f32| div().h(px(10.)).w(gpui::relative(fraction)).rounded(px(4.)).bg(theme::hover());
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(bar(0.3))
        .child(bar(0.7))
        .child(bar(0.9))
        .child(bar(0.5))
        .child(div().mt(px(8.)).h(px(88.)).flex().gap(px(2.)).children((0..3).map(|_| div().flex_1().bg(theme::hover()))))
}

/// The hairline between groups of the pane.
pub fn rule() -> Div {
    div().h(px(1.)).bg(theme::hairline())
}

/// "Open in Discord" with the network's icon and an arrow out.
pub fn open_in(id: impl Into<ElementId>, network: Network) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(theme::ROW_ONE))
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .gap(px(8.))
        .type_ui()
        .cursor_pointer()
        .hover(|row| row.bg(theme::hover()))
        .child(crate::gui::components::network_icon(network, 16.))
        .child(format!("Open in {}", network.label()))
        .child(glyph("icons/external-link.svg", 14., theme::text_soft()))
}

/// The faint line at the foot.
pub fn foot_line(words: String) -> Option<Div> {
    (!words.is_empty()).then(|| div().type_small().text_color(theme::text_faint()).child(words))
}

pub fn presence_line_for(
    person: &Person,
    profile: Option<&protocol::Profile>,
    presence: Presence,
    status: &str,
    in_call: bool,
) -> PresenceLine {
    facts::presence_line(person, profile, presence, status, in_call, crate::view::now())
}
