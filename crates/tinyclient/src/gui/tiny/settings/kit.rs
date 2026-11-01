//! The pieces the Server Settings pages share: a field, a card, a table's head and rows, a role
//! chip, a colour swatch, a switch row, the allow / inherit / deny control of a channel's
//! override, a check box, and a picker's trigger.

use crate::gui::components::{field, glyph};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, ElementId, Entity, Focusable, Rgba, SharedString, Stateful, Window};

/// Now, in unix seconds.
pub(in crate::gui::tiny) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// A one-line text field, `width` wide (or the full width with `None`).
pub(in crate::gui::tiny) fn input(input: &Entity<TextInput>, width: Option<f32>, window: &Window, cx: &mut Context<Shell>) -> Div {
    let focused = input.focus_handle(cx).is_focused(window);
    let field = field(focused).h(px(36.)).child(div().flex_1().min_w_0().child(input.clone()));
    match width {
        Some(width) => field.w(px(width)),
        None => field.w_full(),
    }
}

/// A search field with its glyph.
pub(in crate::gui::tiny) fn search(input: &Entity<TextInput>, width: f32, window: &Window, cx: &mut Context<Shell>) -> Div {
    let focused = input.focus_handle(cx).is_focused(window);
    field(focused).w(px(width)).h(px(32.)).child(glyph("icons/search.svg", 14., theme::text_faint())).child(div().flex_1().min_w_0().child(input.clone()))
}

/// A raised box with a hairline: a group of rows, or a note.
pub(in crate::gui::tiny) fn card() -> Div {
    div().w_full().rounded(px(theme::R_CARD)).bg(theme::raised()).border_1().border_color(theme::hairline()).flex().flex_col()
}

/// A note in a card: a glyph, then words that wrap.
pub(in crate::gui::tiny) fn note(icon: &'static str, tone: Rgba, words: impl Into<SharedString>) -> Div {
    card()
        .p(px(12.))
        .flex_row()
        .items_start()
        .gap(px(10.))
        .child(div().pt(px(2.)).child(glyph(icon, 16., tone)))
        .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text_soft()).child(words.into()))
}

/// A table's head: labels at the widths their columns take (`None` is the flexible one).
pub(in crate::gui::tiny) fn table_head(columns: &[(&'static str, Option<f32>)]) -> Div {
    let mut head = div().h(px(32.)).px(px(12.)).flex().items_center().gap(px(12.)).border_b_1().border_color(theme::hairline());
    for (label, width) in columns {
        let cell = div().type_micro().text_color(theme::text_faint()).child(label.to_uppercase());
        head = head.child(match width {
            Some(width) => cell.flex_none().w(px(*width)),
            None => cell.flex_1().min_w_0(),
        });
    }
    head
}

/// One row of a table, 52 high, lit on hover.
pub(in crate::gui::tiny) fn table_row(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .min_h(px(52.))
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(12.))
        .border_b_1()
        .border_color(theme::hairline())
        .when(on, |row| row.bg(theme::accent_wash()))
        .when(!on, |row| row.hover(|style| style.bg(theme::hover())))
}

/// A cell `width` wide, one line of small text that truncates.
pub(in crate::gui::tiny) fn cell(width: f32, words: impl Into<SharedString>, tone: Rgba) -> Div {
    div().flex_none().w(px(width)).min_w_0().truncate().type_small().text_color(tone).child(words.into())
}

/// A role as a chip: its colour's dot, then its name.
pub(in crate::gui::tiny) fn role_chip(name: impl Into<SharedString>, color: Option<u32>) -> Div {
    div()
        .flex_none()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(11.))
        .bg(theme::hover())
        .border_1()
        .border_color(theme::hairline())
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().size(px(8.)).rounded_full().bg(color.map(gpui::rgb).unwrap_or(theme::text_faint())))
        .child(div().type_small().text_color(theme::text()).child(name.into()))
}

/// A colour to pick: a round swatch, ringed when chosen. `None` is no colour, struck through.
pub(in crate::gui::tiny) fn swatch(id: impl Into<ElementId>, color: Option<u32>, on: bool) -> Stateful<Div> {
    let ring = div()
        .id(id)
        .flex_none()
        .size(px(28.))
        .rounded_full()
        .p(px(3.))
        .border_2()
        .border_color(if on { theme::text() } else { gpui::rgba(0x00000000) })
        .cursor_pointer()
        .hover(|style| style.border_color(theme::border_strong()));
    let inner = div().size_full().rounded_full();
    ring.child(match color {
        Some(color) => inner.bg(gpui::rgb(color)),
        None => inner.border_1().border_color(theme::border_strong()).flex().items_center().justify_center().child(glyph("icons/minus.svg", 14., theme::text_faint())),
    })
}

/// A check box, 16 square. `None` is mixed (some of a selection).
pub(in crate::gui::tiny) fn check_box(id: impl Into<ElementId>, on: Option<bool>, enabled: bool) -> Stateful<Div> {
    let filled = on != Some(false);
    div()
        .id(id)
        .flex_none()
        .size(px(16.))
        .rounded(px(theme::R_CHIP))
        .border_1()
        .border_color(if filled { theme::accent() } else { theme::border_strong() })
        .when(filled, |check| check.bg(theme::accent()))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |check| check.cursor_pointer())
        .when(!enabled, |check| check.opacity(0.35))
        .children(match on {
            Some(true) => Some(glyph("icons/check.svg", 12., theme::on_accent())),
            None => Some(glyph("icons/minus.svg", 12., theme::on_accent())),
            Some(false) => None,
        })
}

/// A setting's row: its name and one line of help, the control at the right. A row that can't
/// be changed is drawn faint and takes no clicks.
pub(in crate::gui::tiny) fn switch_row(id: impl Into<ElementId>, label: &'static str, help: &'static str, on: bool, enabled: bool) -> Stateful<Div> {
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .min_h(px(56.))
        .px(px(14.))
        .py(px(10.))
        .flex()
        .items_center()
        .gap(px(16.))
        .border_b_1()
        .border_color(theme::hairline())
        .when(enabled, |row| row.cursor_pointer().hover(|style| style.bg(theme::hover())))
        .when(!enabled, |row| row.opacity(0.5))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(label))
                .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(help)),
        )
        .child(crate::gui::settings::controls::switch(SharedString::from(format!("{id}-switch")), label, on))
}

/// One of an override's three states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Tri {
    Deny,
    Inherit,
    Allow,
}

/// One cell of the allow / inherit / deny control: a cross, a slash or a check, lit in red, grey
/// or green when it is the state.
pub(in crate::gui::tiny) fn tri_cell(id: impl Into<ElementId>, tri: Tri, on: bool, enabled: bool) -> Stateful<Div> {
    let (icon, tone, fill) = match tri {
        Tri::Deny => ("icons/x.svg", theme::danger(), theme::danger_soft()),
        Tri::Inherit => ("icons/minus.svg", theme::text(), theme::selected()),
        Tri::Allow => ("icons/check.svg", theme::positive(), theme::positive_soft()),
    };
    div()
        .id(id)
        .size(px(28.))
        .rounded(px(theme::R_CHIP))
        .flex()
        .items_center()
        .justify_center()
        .when(on, |cell| cell.bg(fill))
        .when(enabled && !on, |cell| cell.cursor_pointer().hover(|style| style.bg(theme::hover())))
        .child(glyph(icon, 14., if on { tone } else { theme::text_faint() }))
}

/// A picker's closed face: a glyph, what is chosen, a chevron.
pub(in crate::gui::tiny) fn picker(id: impl Into<ElementId>, icon: &'static str, words: impl Into<SharedString>, open: bool, width: f32) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(width))
        .h(px(32.))
        .px(px(10.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::field())
        .border_1()
        .border_color(if open { theme::focus() } else { theme::border() })
        .hover(|style| style.border_color(theme::border_strong()))
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .child(glyph(icon, 14., theme::text_faint()))
        .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(words.into()))
        .child(glyph("icons/chevron-down.svg", 14., theme::text_faint()))
}

/// A small round count, as `3`, beside a heading.
pub(in crate::gui::tiny) fn count(n: usize) -> Div {
    div().flex_none().h(px(18.)).px(px(6.)).rounded(px(9.)).bg(theme::hover()).flex().items_center().type_micro().text_color(theme::text_soft()).child(n.to_string())
}

/// A page's empty list: a glyph and a line.
pub(in crate::gui::tiny) fn empty(icon: &'static str, words: impl Into<SharedString>) -> Div {
    card().py(px(28.)).items_center().gap(px(8.)).child(glyph(icon, 20., theme::text_faint())).child(div().type_ui().text_color(theme::text_soft()).child(words.into()))
}
