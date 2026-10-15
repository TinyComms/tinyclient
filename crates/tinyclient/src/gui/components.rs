//! The pieces every screen is built from (`docs/design/01-system.md`, section 3). All four
//! networks use the same ones. A component is a function that returns an element in its resting
//! look with its hover, pressed and focus states attached; the caller adds the click.
//!
//! Overlays are placed with [`placed`] (`components/place.rs`), which keeps every one inside the
//! window.

// Not every piece has a caller in every build; they are the system the other views draw from.
#![allow(dead_code)]

mod avatar;
mod overlay;
pub mod place;

#[allow(unused_imports)]
pub use avatar::{group_faces, initials, pair, pair_or, presence_dot, square, tint_of, Avatar, Shape};
#[allow(unused_imports)]
pub use overlay::*;
#[allow(unused_imports)]
pub use place::{placed, Layer, Place};

use super::assets::{glyph_asset, network_asset};
use super::theme::{self, Type};
use gpui::{
    div, img, prelude::*, px, svg, Div, ElementId, Hsla, Img, Rgba, SharedString, Stateful, Svg,
};
use protocol::{Badge, Conversation, ConversationKind, Link, Network};

/// A monochrome interface glyph, drawn with a 1.5 pt stroke at any size.
pub fn glyph(name: &'static str, size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(glyph_asset(name, size))
        .size(px(size))
        .flex_none()
        .text_color(color.into())
}

/// The network's own icon, in its own colors. Never tinted or put on a plate. Only on the rail,
/// the conversation strip, the account menu and sign-in (C5).
pub fn network_icon(network: Network, size: f32) -> Img {
    img(network_asset(network, size)).size(px(size)).flex_none()
}

/// The network's mark in one color, for a list that mixes networks while two or more are signed
/// in: 14, `text_faint`, leading the title (C5). The logo's shape drawn as a mask.
pub fn network_mark(network: Network, size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(network_asset(network, size)).size(px(size)).flex_none().text_color(color.into())
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

/// Messages to me (a DM, a group DM): an unfilled count, the numeral `text_soft` inside a 1 pt
/// `border_strong` ring (`polish/00-rulings.md`: outlined, never filled). [`badge_count_unread`]
/// on a row that is unread.
pub fn badge_count(count: u32) -> Div {
    badge_shell(counted(count)).border_1().border_color(theme::border_strong()).text_color(theme::text_soft())
}

/// The unfilled count on an unread row: `text`.
pub fn badge_count_unread(count: u32) -> Div {
    badge_count(count).text_color(theme::text())
}

/// A mention of me: the one filled pill, accent, the numeral alone. Also on a muted row.
pub fn badge_mention(count: u32) -> Div {
    badge_shell(counted(count)).bg(theme::mention()).text_color(theme::on_accent())
}

/// A mention inside something muted keeps the same accent pill (C6); the name stays for callers.
pub fn badge_quiet(count: u32) -> Div {
    badge_mention(count)
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
        // Unread is the title at 600 and the preview lifted to `text`; the time stays faint (C1, C2).
        Badge::Count(_) | Badge::Dot => tone(theme::text(), theme::SEMIBOLD, theme::text(), faint),
        Badge::None => {
            let title = if one_line && !selected { theme::text_soft() } else { theme::text() };
            tone(title, theme::REGULAR, theme::text_soft(), faint)
        }
    }
}

/// What a conversation's badge slot says (C6), the one rule every list, the switcher and the
/// folder tabs draw from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeLook {
    None,
    /// A mention of me: the filled accent pill. Wins over everything, muted included.
    Mention(u32),
    /// Messages to me in a DM or group DM: the outlined count.
    Count(u32),
    /// Anything else unread.
    Dot,
}

pub fn badge_look(conversation: &Conversation) -> BadgeLook {
    // A one-to-one DM is all to me, so its mentions are its count; elsewhere a mention is the pill.
    let pill = conversation.mentions > 0 && (conversation.kind != ConversationKind::Direct || conversation.muted);
    if pill {
        return BadgeLook::Mention(conversation.mentions);
    }
    match (conversation.badge(), conversation.muted) {
        (_, true) | (Badge::None, _) => BadgeLook::None,
        (Badge::Count(count), false) => BadgeLook::Count(count),
        (Badge::Dot, false) => BadgeLook::Dot,
    }
}

/// The element for a [`BadgeLook`].
pub fn badge_for(look: BadgeLook) -> Option<Div> {
    match look {
        BadgeLook::None => None,
        BadgeLook::Mention(count) => Some(badge_mention(count)),
        BadgeLook::Count(count) => Some(badge_count_unread(count)),
        BadgeLook::Dot => Some(badge_dot(8., theme::unread())),
    }
}

/// What sits in a conversation row's badge slot (C6): a mention of me is the accent pill, messages
/// to me an outlined count, anything else unread a dot. Muted shows nothing but a mention.
pub fn row_badge(conversation: &Conversation) -> Option<Div> {
    badge_for(badge_look(conversation))
}

/// A roll-up on a Space row, a collapsed group, a network or pinned rail button: a dot, accent
/// when a mention is inside (C6). The one number is the Inbox's, a [`badge_mention`].
pub fn rollup_badge(attention: u32, unread: bool) -> Option<Div> {
    match (attention, unread) {
        (0, false) => None,
        (0, true) => Some(badge_dot(8., theme::unread())),
        (_, _) => Some(badge_dot(8., theme::mention())),
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
            // Rows abut, so the keyboard's ring is inset, over any fill.
            .when(focused, |row| row.shadow(theme::focus_ring_inset())),
    )
}

/// A list's section header (`polish/01-system.md` §2): 28 tall (24 for the first in a list),
/// label 11/500 `text_faint` at the text start, sitting 8 above the first row, so the gap above
/// it is the group's spacing. No count, no rule, no fill. The caller appends a chevron or a dot
/// into its right slot when the section folds.
pub fn section_header(label: impl Into<SharedString>, first: bool) -> Div {
    let height = if first { theme::SECTION_FIRST } else { theme::SECTION };
    div()
        .flex_none()
        .w_full()
        .h(px(height))
        .px(px(theme::PANE_PAD))
        .pb(px(4.))
        .flex()
        .items_end()
        .gap(px(4.))
        .type_micro()
        .text_color(theme::text_faint())
        .child(div().min_w_0().truncate().child(label.into()))
}

/// A collapsible [`section_header`]: the 12 chevron right of the label shows while collapsed (and
/// on hover, by the caller's group), and a collapsed section with hidden unread shows its dot at
/// the right.
pub fn section_header_folding(label: impl Into<SharedString>, first: bool, collapsed: bool, hidden_unread: Option<Div>) -> Div {
    let chevron = if collapsed { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" };
    section_header(label, first)
        .pr(px(theme::PANE_PAD))
        .child(glyph(chevron, 12., theme::text_faint()).when(!collapsed, |chevron| chevron.opacity(0.)))
        .child(div().flex_1())
        .children(hidden_unread.filter(|_| collapsed))
}

/// The title strip that starts every pane: 52 high, title 16/600 at x 16, actions as 28 icon
/// buttons 4 apart and 12 from the right. Pass `rule` for conversation and right-pane strips,
/// which have a hairline under them; the sidebar's has none. Make it drag the window with
/// `layout::drags_window`.
pub fn title_strip(title: impl Into<SharedString>, rule: bool) -> Div {
    div()
        .flex_none()
        .h(px(theme::STRIP))
        .pl(px(theme::PANE_PAD))
        .pr(px(12.))
        .flex()
        .items_center()
        .gap(px(4.))
        .when(rule, |strip| strip.border_b_1().border_color(theme::hairline()))
        .child(pane_title(title))
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
    /// The link is in trouble by itself and is trying again: reconnecting, starting.
    Warning,
    /// The network wants something only its own app can do.
    Attention,
    Danger,
    /// Waiting on macOS's keychain prompt.
    Keychain,
}

/// Which banner wins when several apply: at most one per pane (`polish/01-system.md` §2).
/// Lower is stronger: service > account needs you > signed out > reconnecting > offline.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum BannerRank {
    Service,
    NeedsYou,
    SignedOut,
    Reconnecting,
    Offline,
}

/// One line under a pane's strip, inset 8: 32 high, `raised` with a hairline, the glyph alone in
/// the tone's color, words that truncate. Shown only while the state is abnormal. Append a
/// [`banner_action`] for the one thing the user can do about it; give it a tooltip with the full
/// words where they may not fit (`.id(..).tooltip(tip(words, None))`).
pub fn banner(tone: Tone, words: impl Into<SharedString>) -> Div {
    let (icon, color) = match tone {
        Tone::Info => ("icons/info.svg", theme::text_soft()),
        Tone::Warning => ("icons/refresh-cw.svg", theme::text_soft()),
        Tone::Attention => ("icons/circle-alert.svg", theme::warning()),
        Tone::Danger => ("icons/circle-alert.svg", theme::danger()),
        Tone::Keychain => ("icons/key.svg", theme::text_soft()),
    };
    div()
        .flex_none()
        .h(px(theme::BANNER))
        .pl(px(10.))
        .pr(px(4.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::hairline())
        .flex()
        .items_center()
        .gap(px(8.))
        .type_ui()
        .text_color(theme::text())
        .child(glyph(icon, 16., color))
        .child(div().flex_1().min_w_0().truncate().child(words.into()))
}

pub fn banner_action(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    button(id, ButtonKind::Ghost, label).h(px(24.)).px(px(8.)).text_color(theme::text()).whitespace_nowrap()
}

/// A full-pane message: glyph 20 `text_faint`, title 16/600, one line 13 `text_soft`, centered at
/// 45% of the region's height. Add at most one secondary button with [`empty_state_with`]. No
/// disc, no illustration.
pub fn empty_state(icon: &'static str, title: &str, detail: &str) -> Div {
    empty_state_column(glyph(icon, 20., theme::text_faint()), title, detail)
}

/// [`empty_state`] with its one action under the line.
pub fn empty_state_with(icon: &'static str, title: &str, detail: &str, action: impl IntoElement) -> Div {
    let column = empty_state_body(glyph(icon, 20., theme::text_faint()), title, detail).child(div().mt(px(12.)).child(action));
    centered_high(column)
}

fn empty_state_body(icon: Svg, title: &str, detail: &str) -> Div {
    div()
        .flex_none()
        .px(px(theme::PANE_PAD))
        .flex()
        .flex_col()
        .items_center()
        .child(icon)
        .child(div().mt(px(12.)).type_title().text_center().text_color(theme::text()).child(title.to_owned()))
        .when(!detail.is_empty(), |body| {
            body.child(
                div()
                    .mt(px(4.))
                    .max_w(px(320.))
                    .text_center()
                    .type_ui()
                    .text_color(theme::text_soft())
                    .child(detail.to_owned()),
            )
        })
}

fn empty_state_column(icon: Svg, title: &str, detail: &str) -> Div {
    centered_high(empty_state_body(icon, title, detail))
}

/// Puts `content` a little above the middle: the free height splits 45 above to 55 below.
fn centered_high(content: Div) -> Div {
    let spacer = |grow: f32| {
        let mut spacer = div().flex_basis(px(0.)).min_h(px(0.));
        spacer.style().flex_grow = Some(grow);
        spacer
    };
    div().size_full().flex().flex_col().items_center().child(spacer(45.)).child(content).child(spacer(55.))
}

/// The title inside a pane's 52 pt strip: 16/600, one line. See [`title_strip`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Key;

    fn row(kind: ConversationKind, unread: u32, mentions: u32, muted: bool) -> Conversation {
        let key = Key { network: Network::Telegram, account: "me".into(), id: "c".into() };
        Conversation { unread, mentions, muted, ..Conversation::new(key, kind, "Fernhill") }
    }

    #[test]
    fn a_mention_is_the_pill_wherever_it_is_and_messages_to_me_are_a_count() {
        use BadgeLook::*;
        // The group of the UAT finding: unread 7, two of them name me. Mentions win.
        assert_eq!(badge_look(&row(ConversationKind::Group, 7, 2, false)), Mention(2));
        assert_eq!(badge_look(&row(ConversationKind::Group, 7, 0, false)), Count(7));
        assert_eq!(badge_look(&row(ConversationKind::Direct, 3, 0, false)), Count(3));
        assert_eq!(badge_look(&row(ConversationKind::Direct, 3, 3, false)), Count(3));
        assert_eq!(badge_look(&row(ConversationKind::Channel, 5, 0, false)), Dot);
        assert_eq!(badge_look(&row(ConversationKind::Channel, 5, 1, false)), Mention(1));
        // Muted: nothing, except a mention, which keeps the same pill.
        assert_eq!(badge_look(&row(ConversationKind::Group, 7, 0, true)), None);
        assert_eq!(badge_look(&row(ConversationKind::Direct, 3, 0, true)), None);
        assert_eq!(badge_look(&row(ConversationKind::Group, 7, 1, true)), Mention(1));
        assert_eq!(badge_look(&row(ConversationKind::Direct, 3, 1, true)), Mention(1));
        assert_eq!(badge_look(&row(ConversationKind::Channel, 0, 0, false)), None);
    }
}
