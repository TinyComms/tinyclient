//! `:` completion: two letters after a colon that begins a word offer emoji, the account's own
//! among them. Never inside a link or after a digit (`10:30`).

use super::picker::emoji::{self, Cell, Custom};
use crate::gui::components::menu;
use crate::gui::theme::{self, Type};
use crate::gui::anim::{self, Play};
use gpui::{div, prelude::*, px, App, Div, MouseButton, MouseDownEvent, ObjectFit, RenderImage, Window};
use std::sync::Arc;

pub const ROWS: usize = 8;
pub const WIDTH: f32 = 340.;
const ROW: f32 = 28.;

/// The colon at `start` and what follows it up to the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub start: usize,
    pub query: String,
}

pub fn trigger(text: &str, caret: usize) -> Option<Trigger> {
    let before = text.get(..caret)?;
    let word_start = before.char_indices().rev().take_while(|(_, ch)| ch.is_alphanumeric() || matches!(ch, '_' | '+' | '-')).last().map_or(caret, |(ix, _)| ix);
    let start = word_start.checked_sub(1)?;
    if before.as_bytes().get(start) != Some(&b':') {
        return None;
    }
    let query = &before[word_start..];
    // A colon right after a letter or digit is a time or a scheme ("10:30", "https:").
    let lead = before[..start].chars().next_back();
    if lead.is_some_and(|ch| !ch.is_whitespace() && !matches!(ch, '(' | '[' | '"' | '\'')) {
        return None;
    }
    // Inside a link that has spaces around it is excluded by the rule above; a word that
    // already closed (`:tada:`) is done.
    (query.chars().count() >= 2 && !query.starts_with(|ch: char| ch.is_ascii_digit())).then(|| Trigger { start, query: query.to_owned() })
}

pub struct List {
    pub trigger: Trigger,
    pub cells: Vec<Cell>,
    pub selected: usize,
}

impl List {
    pub fn new(trigger: Trigger, recent: &[String], customs: &[Custom]) -> List {
        let cells = emoji::search(&trigger.query, recent, customs, ROWS);
        List { trigger, cells, selected: 0 }
    }

    pub fn step(&mut self, down: bool) {
        let count = self.cells.len();
        if count > 0 {
            self.selected = if down { (self.selected + 1) % count } else { (self.selected + count - 1) % count };
        }
    }
}

/// The popover: 340 wide, 28 pt rows, the selected one filled. `custom_image` decodes a custom
/// emoji's still; `pick` is told the row clicked.
pub fn draw(
    list: &List,
    customs: &[Custom],
    tone: usize,
    mut custom_image: impl FnMut(&Custom) -> Option<Arc<RenderImage>>,
    pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    menu().w(px(WIDTH)).max_w(px(WIDTH)).children(list.cells.iter().enumerate().map(|(ix, cell)| {
        let custom = match cell {
            Cell::Custom(at) => customs.get(*at as usize),
            _ => None,
        };
        let face = match custom {
            Some(custom) => match custom_image(custom) {
                Some(image) => div().size(px(20.)).child(anim::picture(image, ObjectFit::Fill, 0., Play::Inline)).into_any_element(),
                None => div().size(px(20.)).rounded(px(4.)).bg(theme::raised()).into_any_element(),
            },
            None => div().w(px(22.)).text_size(px(18.)).line_height(px(22.)).child(emoji::text_of(cell, tone, customs)).into_any_element(),
        };
        let pick = pick.clone();
        div()
            .id(("colon", ix))
            .h(px(ROW))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .when(ix == list.selected, |row| row.bg(theme::selected()))
            .hover(|style| style.bg(theme::hover()))
            // Mouse-down, so the field keeps its focus and caret.
            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                pick(ix, window, cx);
            })
            .child(face)
            .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(emoji::name_of(cell, customs)))
            .children(custom.filter(|custom| !custom.pack.is_empty()).map(|custom| div().flex_none().type_small().text_color(theme::text_soft()).child(custom.pack.clone())))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colon_completes_only_after_two_letters_at_the_start_of_a_word() {
        assert_eq!(trigger("We found it :tad", 16), Some(Trigger { start: 12, query: "tad".into() }));
        assert_eq!(trigger(":ta", 3), Some(Trigger { start: 0, query: "ta".into() }));
        assert_eq!(trigger("(:+1", 4), Some(Trigger { start: 1, query: "+1".into() }));
        assert_eq!(trigger("so :t", 5), None, "one letter is not enough");
        assert_eq!(trigger("at 10:30", 8), None, "a time");
        assert_eq!(trigger("see https://example.com", 23), None, "a link");
        assert_eq!(trigger("a:bc", 4), None, "inside a word");
        assert_eq!(trigger("done :tada: ", 12), None);
        assert_eq!(trigger("ok :12", 6), None);
        // The caret mid-text completes what is before it.
        assert_eq!(trigger("x :smi more", 6), Some(Trigger { start: 2, query: "smi".into() }));
        assert_eq!(trigger("é :ça", "é :ça".len()), Some(Trigger { start: 3, query: "ça".into() }));
        assert_eq!(trigger("abc", 99), None);
    }

    #[test]
    fn the_list_ranks_the_exact_name_first_and_wraps() {
        let customs = [Custom { name: ":tada_marten:".into(), id: "1".into(), file: None, pack: "Moss & Lichen".into(), animated: false }];
        let mut list = List::new(Trigger { start: 0, query: "tada".into() }, &[], &customs);
        assert_eq!(emoji::text_of(&list.cells[0], 0, &customs), "🎉");
        assert!(list.cells.len() <= ROWS && list.cells.len() >= 2);
        list.step(false);
        assert_eq!(list.selected, list.cells.len() - 1);
        list.step(true);
        assert_eq!(list.selected, 0);
    }
}
