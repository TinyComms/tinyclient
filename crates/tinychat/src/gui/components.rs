//! The pieces every screen is built from (`docs/design/01-system.md`, section 3). All four
//! networks use the same ones. A component is a function that returns an element in its resting
//! look with its hover, pressed and focus states attached; the caller adds the click.

mod avatar;
mod overlay;

// Not every piece has a caller in every build; they are the system the other views draw from.
#[allow(unused_imports)]
pub use avatar::{initials, pair, presence_dot, tint_of, Avatar, Shape};
#[allow(unused_imports)]
pub use overlay::*;

use super::assets::{glyph_asset, network_asset};
use super::theme::{self, Type};
use gpui::{
    div, img, prelude::*, px, svg, Div, ElementId, Hsla, Img, Rgba, SharedString, Stateful, Svg,
};
use protocol::{Badge, Conversation, Link, Network};

/// A monochrome interface glyph, drawn with a 1.5 pt stroke at any size.
pub fn glyph(name: &'static str, size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(glyph_asset(name, size))
        .size(px(size))
        .flex_none()
        .text_color(color.into())
}

/// The network's own icon, in its own colors. Never tinted or put on a plate.
pub fn network_icon(network: Network, size: f32) -> Img {
    img(network_asset(network, size)).size(px(size)).flex_none()
}

// Badges.

fn badge_shell(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .min_w(px(16.))
        .px(px(5.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::SEMIBOLD)
        .child(label.into())
}

fn counted(count: u32) -> String {
    match count {
        0..=99 => count.to_string(),
        _ => "99+".to_owned(),
    }
}

/// Unread, neutral: someone is waiting, but nobody named me.
pub fn badge_count(count: u32) -> Div {
    badge_shell(counted(count)).bg(theme::count_fill()).text_color(theme::text())
}

/// For me: the one accent badge.
pub fn badge_mention(count: u32) -> Div {
    badge_shell(counted(count)).bg(theme::mention()).text_color(theme::on_accent())
}

/// A mention in something muted, and a roll-up made only of those.
pub fn badge_quiet(count: u32) -> Div {
    badge_shell(counted(count)).bg(theme::quiet_fill()).text_color(theme::text_soft())
}

/// On a conversation's own row a mention says so: `@3`. A roll-up is the bare number, because
/// it also counts direct messages.
fn at(count: u32) -> String {
    format!("@{}", counted(count))
}

/// A dot, centered in the 16 pt slot a badge would take so rows line up.
pub fn badge_dot(diameter: f32, color: impl Into<Hsla>) -> Div {
    div().flex_none().size(px(16.)).flex().items_center().justify_center().child(
        div().size(px(diameter)).rounded(px(diameter / 2.)).bg(color.into()),
    )
}

/// A badge overhanging the top-right of an icon, on a 2 pt ring of the surface behind it.
pub fn badge_on_icon(badge: Div, surface: Rgba) -> Div {
    div()
        .absolute()
        .top(px(-6.))
        .right(px(-8.))
        .rounded(px(10.))
        .border_2()
        .border_color(surface)
        .bg(surface)
        .child(badge)
}

// Rows.

/// The colors and weight of a conversation row: the six states of `06-live-signals.md`, first
/// match wins, in the tokens of `01-system.md`.
pub struct RowTone {
    pub title: Rgba,
    pub weight: gpui::FontWeight,
    pub preview: Rgba,
    pub time: Rgba,
}

/// `one_line` rows rest one level quieter: a tree is for finding a name.
pub fn row_tone(conversation: &Conversation, selected: bool, one_line: bool) -> RowTone {
    // `text_faint` never sits on `selected`; it steps up there.
    let faint = if selected { theme::text_soft() } else { theme::text_faint() };
    let tone = |title, weight, preview, time| RowTone { title, weight, preview, time };
    if conversation.muted {
        return tone(faint, theme::REGULAR, faint, faint);
    }
    match conversation.badge() {
        Badge::Count(_) => tone(theme::text(), theme::SEMIBOLD, theme::text(), theme::text_soft()),
        Badge::Dot => tone(theme::text(), theme::SEMIBOLD, theme::text_soft(), theme::text_soft()),
        Badge::None => {
            let title = if one_line && !selected { theme::text_soft() } else { theme::text() };
            tone(title, theme::REGULAR, theme::text_soft(), faint)
        }
    }
}

/// What sits in a conversation row's badge slot. A number means someone needs me; a dot means
/// there is something new; a muted row shows its bell.
pub fn row_badge(conversation: &Conversation) -> Option<Div> {
    match (conversation.badge(), conversation.muted) {
        (Badge::Count(count), true) => Some(badge_shell(at(count)).bg(theme::quiet_fill()).text_color(theme::text_soft())),
        // A mention is for me: accent. A direct message is someone waiting: a neutral count.
        (Badge::Count(count), false) if conversation.mentions > 0 && !conversation.kind.is_direct() => {
            Some(badge_shell(at(count)).bg(theme::mention()).text_color(theme::on_accent()))
        }
        (Badge::Count(count), false) => Some(badge_count(count)),
        (Badge::Dot, _) => Some(badge_dot(8., theme::unread())),
        (Badge::None, true) => Some(
            div()
                .flex_none()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("icons/bell-off.svg", 12., theme::text_faint())),
        ),
        (Badge::None, false) => None,
    }
}

/// A roll-up on a Space row or a rail button: the number when anything inside needs me, else a
/// dot when anything unmuted is unread.
pub fn rollup_badge(attention: u32, unread: bool) -> Option<Div> {
    match (attention, unread) {
        (0, false) => None,
        (0, true) => Some(badge_dot(8., theme::unread())),
        (count, _) => Some(badge_mention(count)),
    }
}

/// A list row: `height` tall (28 or 48), inset 8 from the pane, radius 6, with its fill for
/// hover and selection and a ring for keyboard focus. `content` is laid out in a row.
pub fn list_row(
    id: impl Into<ElementId>,
    height: f32,
    selected: bool,
    focused: bool,
    content: Div,
) -> Stateful<Div> {
    div().id(id).w_full().h(px(height)).px(px(theme::INSET)).py(px(1.)).cursor_pointer().child(
        content
            .size_full()
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
            .when(selected, |row| row.bg(theme::selected()))
            .when(focused, |row| row.border_2().border_color(theme::focus()).px(px(6.))),
    )
}

/// A static placeholder in a row's final geometry, for a page that has not arrived.
pub fn skeleton(width: f32, height: f32) -> Div {
    div().w(px(width)).h(px(height)).rounded(px(theme::R_CHIP)).bg(theme::hover())
}

// Buttons.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// One per view.
    Primary,
    Secondary,
    Ghost,
    #[allow(dead_code)]
    Danger,
}

/// A state fill made opaque. The focus ring is a shadow with no blur, which is painted under the
/// whole control, so a translucent control would show it as a fill.
fn opaque(fill: Rgba) -> Rgba {
    theme::over(fill, theme::canvas())
}

fn button_shell(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(theme::CONTROL))
        .px(px(12.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .type_ui()
        .font_weight(theme::MEDIUM)
        .child(label.into())
}

/// A 28 pt button. `.h(px(24.))` makes it small, `.h(px(32.))` a dialog's main action.
pub fn button(id: impl Into<ElementId>, kind: ButtonKind, label: impl Into<SharedString>) -> Stateful<Div> {
    let shell = button_shell(label).id(id).cursor_pointer().tab_index(0);
    match kind {
        ButtonKind::Primary => shell
            .bg(theme::accent())
            .text_color(theme::on_accent())
            .hover(|style| style.bg(theme::accent_hover()))
            .active(|style| style.bg(theme::accent_pressed()))
            .focus_visible(|style| style.shadow(theme::focus_ring_gap(theme::canvas()))),
        ButtonKind::Secondary => shell
            .bg(theme::hover())
            .border_1()
            .border_color(theme::border())
            .text_color(theme::text())
            .hover(|style| style.bg(theme::selected()).border_color(theme::border_strong()))
            .active(|style| style.bg(gpui::rgba(0xffffff08)))
            .focus_visible(|style| style.bg(opaque(theme::hover())).shadow(theme::focus_ring())),
        ButtonKind::Ghost => shell
            .text_color(theme::text_soft())
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .active(|style| style.bg(theme::selected()))
            .focus_visible(|style| style.bg(opaque(theme::hover())).shadow(theme::focus_ring())),
        ButtonKind::Danger => shell
            .bg(theme::danger_soft())
            .text_color(theme::danger())
            .hover(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
            .active(|style| style.bg(theme::danger_fill()).text_color(theme::on_accent()))
            .focus_visible(|style| style.bg(opaque(theme::danger_soft())).shadow(theme::focus_ring())),
    }
}

/// A button led by a 16 pt glyph: 10 on the icon side, 12 on the text side.
pub fn button_with_icon(
    id: impl Into<ElementId>,
    kind: ButtonKind,
    icon: &'static str,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let color = match kind {
        ButtonKind::Primary => theme::on_accent(),
        ButtonKind::Danger => theme::danger(),
        ButtonKind::Secondary | ButtonKind::Ghost => theme::text_soft(),
    };
    let label: SharedString = label.into();
    button(id, kind, "").pl(px(10.)).child(glyph(icon, 16., color)).child(label)
}

/// Disabled, and also busy: a busy button says so in words ("Sending…") in this look.
pub fn button_disabled(label: impl Into<SharedString>) -> Div {
    button_shell(label).bg(theme::hover()).text_color(theme::text_disabled())
}

/// A 28 pt square holding a 16 pt glyph; `.size(px(24.))` in a toolbar. Give it a tooltip.
pub fn icon_button(id: impl Into<ElementId>, icon: &'static str, toggled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(theme::CONTROL))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .when(toggled, |button| button.bg(theme::selected()))
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .focus_visible(|style| style.bg(opaque(theme::hover())).shadow(theme::focus_ring()))
        // An SVG does not take its color from the text around it, so hover cannot brighten it.
        .child(glyph(icon, 16., if toggled { theme::text() } else { theme::text_soft() }))
}

// Fields.

/// The box of a text field: the caller puts the input inside. Focus is a `focus` border and a
/// 3 pt halo, always shown, because a field with the caret in it is where typing goes.
pub fn field(focused: bool) -> Div {
    div()
        .h(px(theme::CONTROL))
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::field())
        .border_1()
        .flex()
        .items_center()
        .gap(px(6.))
        .type_ui()
        .map(|field| {
            if focused {
                // Opaque, or the halo would show through the field as a fill.
                field.bg(theme::over(theme::field(), theme::canvas())).border_color(theme::focus()).shadow(theme::field_halo())
            } else {
                field.border_color(theme::border()).hover(|style| style.border_color(theme::border_strong()))
            }
        })
}

/// A field whose value was refused. Put [`field_error_line`] 4 below it.
#[allow(dead_code)]
pub fn field_error() -> Div {
    field(false).border_color(theme::danger())
}

#[allow(dead_code)]
pub fn field_error_line(words: impl Into<SharedString>) -> Div {
    div().mt(px(4.)).type_small().text_color(theme::danger()).child(words.into())
}

/// A keycap such as `⌘K`.
pub fn keycap(keys: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(18.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::MEDIUM)
        .text_color(theme::text_faint())
        .child(keys.into())
}

/// The search field's box, with its glyph. The caller adds the input, then a keycap or a clear.
pub fn search_field(focused: bool) -> Div {
    field(focused).child(glyph("icons/search.svg", 14., theme::text_faint()))
}

// Switches and lines.

/// A segmented switch of two to four [`segment`]s.
pub fn segmented() -> Div {
    div()
        .flex_none()
        .h(px(theme::CONTROL))
        .p(px(2.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::hover())
        .flex()
        .items_center()
}

pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.))
        .px(px(10.))
        .rounded(px(theme::R_CHIP))
        .flex()
        .items_center()
        .cursor_pointer()
        .type_ui()
        .font_weight(theme::MEDIUM)
        .map(|segment| {
            if selected {
                segment.bg(theme::segment()).text_color(theme::text())
            } else {
                segment.text_color(theme::text_soft()).hover(|style| style.text_color(theme::text()))
            }
        })
        .child(label.into())
}

/// A 1 pt line between sections.
pub fn divider() -> Div {
    div().flex_none().h(px(1.)).w_full().bg(theme::hairline())
}

/// The line between panes.
#[allow(dead_code)]
pub fn divider_vertical() -> Div {
    div().flex_none().w(px(1.)).h_full().bg(theme::hairline())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    /// The link is in trouble by itself: reconnecting, unreachable.
    Warning,
    /// The network wants something only its own app can do.
    Attention,
    Danger,
}

/// How a pane says "Slack is offline": shown only while the state is abnormal. Append a
/// [`banner_action`] for the one thing the user can do about it.
pub fn banner(tone: Tone, words: impl Into<SharedString>) -> Div {
    let (icon, color, fill) = match tone {
        Tone::Info => ("icons/info.svg", theme::text_soft(), theme::raised()),
        Tone::Warning => ("icons/wifi-off.svg", theme::warning(), theme::warning_soft()),
        Tone::Attention => ("icons/triangle-alert.svg", theme::warning(), theme::warning_soft()),
        Tone::Danger => ("icons/circle-alert.svg", theme::danger(), theme::danger_soft()),
    };
    div()
        .flex_none()
        // One line is 32 tall; words that do not fit wrap to a second rather than being cut.
        .min_h(px(32.))
        .py(px(4.))
        .pl(px(8.))
        .pr(px(4.))
        .rounded(px(theme::R_CONTROL))
        .bg(fill)
        .when(tone == Tone::Info, |banner| banner.border_1().border_color(theme::hairline()))
        .flex()
        .items_center()
        .gap(px(8.))
        .type_ui()
        .text_color(theme::text())
        .child(glyph(icon, 16., color))
        .child(div().flex_1().min_w_0().child(words.into()))
}

pub fn banner_action(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    button(id, ButtonKind::Ghost, label).h(px(24.)).px(px(8.)).text_color(theme::text()).whitespace_nowrap()
}

/// A full-pane message: glyph, title, one line, and at most one secondary button appended by
/// the caller. No disc, no illustration.
pub fn empty_state(icon: &'static str, title: &str, detail: &str) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(glyph(icon, 20., theme::text_faint()))
        .child(div().mt(px(12.)).type_title().text_color(theme::text()).child(title.to_owned()))
        .child(
            div()
                .mt(px(4.))
                .mb(px(16.))
                .max_w(px(320.))
                .text_center()
                .type_ui()
                .text_color(theme::text_soft())
                .child(detail.to_owned()),
        )
}

/// The 52 pt strip that starts every pane, level with the traffic lights.
pub fn pane_title(title: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().truncate().type_title().text_color(theme::text()).child(title.into())
}

// Links, in words. A connected network says nothing; these are for the abnormal states.

pub fn link_word(link: Link) -> &'static str {
    match link {
        Link::Connected => "Connected",
        Link::Offline => "Reconnecting",
        Link::Expired => "Signed out",
        Link::SignedOut => "Not signed in",
    }
}
