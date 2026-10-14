//! The editing model behind every text field: text, selection, IME marked text and undo. It
//! knows nothing of GPUI, so every key the field answers to can be tested without a window.
//!
//! Offsets are UTF-8 bytes on grapheme boundaries. Movement by visual line asks a [`Rows`] for
//! where the wrapped lines are; the field passes what it last drew.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Undo steps kept. Older ones fall off.
const HISTORY: usize = 100;

/// Where the drawn lines are. A row is one visual line: a hard line, or a piece of one after
/// soft wrapping. Its range leaves out the newline that ends it.
pub trait Rows {
    fn count(&self) -> usize;
    fn range(&self, row: usize) -> Range<usize>;
    fn x_of(&self, row: usize, offset: usize) -> f32;
    fn offset_at(&self, row: usize, x: f32) -> usize;

    /// A caret between two soft-wrapped rows belongs to the start of the later one.
    fn row_of(&self, offset: usize) -> usize {
        let mut found = 0;
        for row in 0..self.count() {
            let range = self.range(row);
            if range.start > offset {
                break;
            }
            found = row;
            if offset < range.end {
                break;
            }
        }
        found
    }
}

/// Rows without a text system: every grapheme is one unit wide, wrapped after `wrap` of them.
/// What the field uses before its first paint, and what the tests use.
pub struct PlainRows {
    rows: Vec<Range<usize>>,
    text: String,
}

impl PlainRows {
    pub fn new(text: &str, wrap: Option<usize>) -> PlainRows {
        let mut rows = Vec::new();
        let mut start = 0;
        for line in text.split('\n') {
            let end = start + line.len();
            match wrap {
                Some(wrap) if wrap > 0 => {
                    let mut row_start = start;
                    for (count, (ix, _)) in line.grapheme_indices(true).enumerate() {
                        if count > 0 && count % wrap == 0 {
                            rows.push(row_start..start + ix);
                            row_start = start + ix;
                        }
                    }
                    rows.push(row_start..end);
                }
                _ => rows.push(start..end),
            }
            start = end + 1;
        }
        PlainRows { rows, text: text.to_owned() }
    }
}

impl Rows for PlainRows {
    fn count(&self) -> usize {
        self.rows.len()
    }

    fn range(&self, row: usize) -> Range<usize> {
        self.rows[row].clone()
    }

    fn x_of(&self, row: usize, offset: usize) -> f32 {
        let range = self.range(row);
        let end = offset.clamp(range.start, range.end);
        self.text[range.start..end].graphemes(true).count() as f32
    }

    fn offset_at(&self, row: usize, x: f32) -> usize {
        let range = self.range(row);
        let column = x.max(0.).round() as usize;
        self.text[range.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map_or(range.end, |(ix, _)| range.start + ix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// The ends of the visual line the caret is on.
    LineStart,
    LineEnd,
    Up,
    Down,
    DocStart,
    DocEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    None,
    Typing,
    Deleting,
    Composing,
}

#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    selection: Range<usize>,
    reversed: bool,
}

#[derive(Debug)]
pub struct Editor {
    text: String,
    selection: Range<usize>,
    /// The caret is at the start of the selection, not its end.
    reversed: bool,
    marked: Option<Range<usize>>,
    multiline: bool,
    /// The column Up and Down keep returning to across short lines.
    goal_x: Option<f32>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Edit,
}

impl Editor {
    pub fn new(multiline: bool) -> Editor {
        Editor {
            text: String::new(),
            selection: 0..0,
            reversed: false,
            marked: None,
            multiline,
            goal_x: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last: Edit::None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    /// Replaces everything and forgets the history: a different draft, not an edit of this one.
    pub fn set_text(&mut self, text: &str) {
        self.text = self.clean(text);
        self.selection = self.text.len()..self.text.len();
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.undo.clear();
        self.redo.clear();
        self.last = Edit::None;
    }

    /// A single-line field holds no line breaks, whatever is pasted into it.
    fn clean(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.multiline {
            text
        } else {
            text.replace('\n', " ")
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
            reversed: self.reversed,
        }
    }

    /// Remembers the state before an edit. Typing joins the step before it until a new word
    /// starts, so one undo takes back one word.
    fn checkpoint(&mut self, edit: Edit, joins: bool) {
        if !(joins && self.last == edit) {
            self.undo.push(self.snapshot());
            if self.undo.len() > HISTORY {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = edit;
    }

    fn splice(&mut self, range: Range<usize>, text: &str) {
        self.text.replace_range(range.clone(), text);
        let caret = range.start + text.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.goal_x = None;
    }

    /// Types or pastes over the selection.
    pub fn insert(&mut self, text: &str) {
        let text = self.clean(text);
        let range = self.marked.take().unwrap_or(self.selection.clone());
        let one_key = text.chars().count() == 1;
        let starts_word = self.text[..range.start].chars().next_back().is_none_or(char::is_whitespace)
            && !text.starts_with(char::is_whitespace);
        self.checkpoint(Edit::Typing, one_key && range.is_empty() && !starts_word);
        self.splice(range, &text);
    }

    /// Puts `text` in place of `range`, as one step: a completion taking the place of what was
    /// typed to find it.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let text = self.clean(text);
        let end = self.snap(range.end);
        let range = self.snap(range.start.min(end))..end;
        self.marked = None;
        self.checkpoint(Edit::None, false);
        self.splice(range, &text);
        self.last = Edit::None;
    }

    /// Shift-Return. A single-line field has no second line to go to.
    pub fn newline(&mut self) -> bool {
        if !self.multiline {
            return false;
        }
        self.checkpoint(Edit::None, false);
        self.splice(self.selection.clone(), "\n");
        true
    }

    /// Removes the selection, or what `motion` would cross from the caret. False when there
    /// was nothing to remove.
    pub fn delete(&mut self, motion: Motion, rows: &dyn Rows) -> bool {
        self.marked = None;
        let range = if self.selection.is_empty() {
            let caret = self.cursor();
            let to = self.target(motion, rows);
            caret.min(to)..caret.max(to)
        } else {
            self.selection.clone()
        };
        if range.is_empty() {
            return false;
        }
        let one_key = range.len() == self.grapheme_len(range.start) && self.selection.is_empty();
        self.checkpoint(Edit::Deleting, one_key);
        self.splice(range, "");
        true
    }

    /// Ctrl-K: to the end of the hard line, or the line break itself when already there.
    pub fn delete_to_line_end(&mut self) -> bool {
        let caret = self.cursor();
        let end = self.text[caret..].find('\n').map_or(self.text.len(), |ix| caret + ix);
        let end = if end == caret { self.next_boundary(caret) } else { end };
        if end == caret {
            return false;
        }
        self.marked = None;
        self.checkpoint(Edit::None, false);
        self.splice(caret..end, "");
        true
    }

    pub fn cut(&mut self) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        let cut = self.selected_text().to_owned();
        self.marked = None;
        self.checkpoint(Edit::None, false);
        self.splice(self.selection.clone(), "");
        Some(cut)
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(previous);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        true
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.reversed = snapshot.reversed;
        self.marked = None;
        self.goal_x = None;
        self.last = Edit::None;
    }

    /// Moves the caret, or with `extend` the end of the selection it is on.
    pub fn go(&mut self, motion: Motion, extend: bool, rows: &dyn Rows) {
        let vertical = matches!(motion, Motion::Up | Motion::Down);
        let to = if !extend && !self.selection.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
            // An arrow key leaves a selection at the end it points to.
            if motion == Motion::Left {
                self.selection.start
            } else {
                self.selection.end
            }
        } else {
            self.target(motion, rows)
        };
        // Up and Down remember the column they started from.
        let goal = self.goal_x.filter(|_| vertical);
        if extend {
            self.select_to(to);
        } else {
            self.place(to);
        }
        self.goal_x = goal;
        self.last = Edit::None;
    }

    fn target(&mut self, motion: Motion, rows: &dyn Rows) -> usize {
        let caret = self.cursor();
        match motion {
            Motion::Left => self.previous_boundary(caret),
            Motion::Right => self.next_boundary(caret),
            Motion::WordLeft => self.word_left(caret),
            Motion::WordRight => self.word_right(caret),
            Motion::DocStart => 0,
            Motion::DocEnd => self.text.len(),
            Motion::LineStart => rows.range(rows.row_of(caret)).start,
            Motion::LineEnd => rows.range(rows.row_of(caret)).end,
            Motion::Up | Motion::Down => {
                let row = rows.row_of(caret);
                let x = *self.goal_x.get_or_insert_with(|| rows.x_of(row, caret));
                match motion {
                    // On the first line Up goes to the start; on the last, Down to the end.
                    Motion::Up if row == 0 => 0,
                    Motion::Up => rows.offset_at(row - 1, x),
                    _ if row + 1 >= rows.count() => self.text.len(),
                    _ => rows.offset_at(row + 1, x),
                }
            }
        }
    }

    pub fn place(&mut self, offset: usize) {
        let offset = self.snap(offset);
        self.selection = offset..offset;
        self.reversed = false;
        self.goal_x = None;
    }

    pub fn select_to(&mut self, offset: usize) {
        let offset = self.snap(offset);
        let anchor = if self.reversed { self.selection.end } else { self.selection.start };
        self.reversed = offset < anchor;
        self.selection = anchor.min(offset)..anchor.max(offset);
        self.goal_x = None;
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        self.goal_x = None;
    }

    /// Double-click: the word under the pointer, or the run of spaces or symbols there.
    pub fn select_word_at(&mut self, offset: usize) {
        let offset = self.snap(offset);
        let word = self
            .text
            .split_word_bound_indices()
            .map(|(start, word)| start..start + word.len())
            .find(|range| range.start <= offset && offset < range.end)
            .unwrap_or(offset..offset);
        self.selection = word;
        self.reversed = false;
        self.goal_x = None;
    }

    /// Triple-click: the hard line under the pointer.
    pub fn select_line_at(&mut self, offset: usize) {
        let offset = self.snap(offset);
        let start = self.text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
        let end = self.text[offset..].find('\n').map_or(self.text.len(), |ix| offset + ix);
        self.selection = start..end;
        self.reversed = false;
        self.goal_x = None;
    }

    /// The input method replaces a range (its marked text when it names none) and is done.
    pub fn ime_replace(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let range = range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let composing = self.marked.take().is_some();
        if composing {
            // The composition's first keystroke already made the undo step.
            self.last = Edit::None;
            let text = self.clean(text);
            self.splice(range, &text);
        } else {
            self.selection = range;
            self.insert(text);
        }
    }

    /// The input method shows text it has not settled on. `selected_utf16` is relative to the
    /// start of that text.
    pub fn ime_mark(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        let range = range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.checkpoint(Edit::Composing, true);
        let text = self.clean(text);
        self.splice(range.clone(), &text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        if let Some(selected) = selected_utf16 {
            let start = range.start + utf8_offset(&text, selected.start);
            let end = range.start + utf8_offset(&text, selected.end);
            self.selection = start..end;
        }
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    pub fn previous_boundary(&self, offset: usize) -> usize {
        self.text[..offset].grapheme_indices(true).next_back().map_or(0, |(ix, _)| ix)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        offset + self.grapheme_len(offset)
    }

    fn grapheme_len(&self, offset: usize) -> usize {
        self.text[offset..].graphemes(true).next().map_or(0, str::len)
    }

    /// The nearest grapheme boundary at or before `offset`.
    fn snap(&self, offset: usize) -> usize {
        if offset >= self.text.len() {
            return self.text.len();
        }
        self.text
            .grapheme_indices(true)
            .map(|(ix, _)| ix)
            .take_while(|ix| *ix <= offset)
            .last()
            .unwrap_or(0)
    }

    fn words(&self) -> impl DoubleEndedIterator<Item = Range<usize>> + '_ {
        self.text
            .split_word_bound_indices()
            .filter(|(_, word)| word.chars().any(char::is_alphanumeric))
            .map(|(start, word)| start..start + word.len())
    }

    fn word_left(&self, offset: usize) -> usize {
        self.words().rev().find(|word| word.start < offset).map_or(0, |word| word.start)
    }

    fn word_right(&self, offset: usize) -> usize {
        self.words().find(|word| word.end > offset).map_or(self.text.len(), |word| word.end)
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())].encode_utf16().count()
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        utf8_offset(&self.text, offset)
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

fn utf8_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (ix, ch) in text.char_indices() {
        if units >= utf16 {
            return ix;
        }
        units += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Editor {
        let mut editor = Editor::new(true);
        for ch in text.chars() {
            if ch == '\n' {
                editor.newline();
            } else {
                editor.insert(&ch.to_string());
            }
        }
        editor
    }

    fn rows(editor: &Editor) -> PlainRows {
        PlainRows::new(editor.text(), None)
    }

    fn go(editor: &mut Editor, motion: Motion, extend: bool) {
        let rows = rows(editor);
        editor.go(motion, extend, &rows);
    }

    #[test]
    fn typing_inserts_at_the_caret_and_over_a_selection() {
        let mut editor = typed("helo");
        go(&mut editor, Motion::Left, false);
        editor.insert("l");
        assert_eq!(editor.text(), "hello");
        assert_eq!(editor.selection(), 4..4);
        editor.select_all();
        editor.insert("x");
        assert_eq!((editor.text(), editor.selection()), ("x", 1..1));
    }

    #[test]
    fn shift_return_breaks_the_line_only_in_a_multiline_field() {
        let mut editor = typed("one");
        assert!(editor.newline());
        editor.insert("two");
        assert_eq!(editor.text(), "one\ntwo");
        assert_eq!(editor.selection(), 7..7);

        let mut single = Editor::new(false);
        single.insert("one");
        assert!(!single.newline());
        single.insert("a\nb\r\nc");
        assert_eq!(single.text(), "onea b c");
    }

    #[test]
    fn arrows_move_by_grapheme_not_by_byte_or_scalar() {
        // é as e + combining accent, a flag (two scalars), a family (seven scalars), a skin tone.
        let mut editor = typed("");
        editor.insert("e\u{301}🇵🇹👨‍👩‍👧👍🏽");
        let ends = [3, 11, 29, 37];
        go(&mut editor, Motion::DocStart, false);
        for end in ends {
            go(&mut editor, Motion::Right, false);
            assert_eq!(editor.selection(), end..end);
        }
        for start in [29, 11, 3, 0] {
            go(&mut editor, Motion::Left, false);
            assert_eq!(editor.selection(), start..start);
        }
    }

    #[test]
    fn backspace_and_delete_take_one_whole_grapheme() {
        let mut editor = typed("");
        editor.insert("a👨‍👩‍👧b");
        let rows = rows(&editor);
        go(&mut editor, Motion::Left, false);
        assert!(editor.delete(Motion::Left, &rows));
        assert_eq!((editor.text(), editor.selection()), ("ab", 1..1));
        assert!(editor.delete(Motion::Right, &rows));
        assert_eq!(editor.text(), "a");
        assert!(!editor.delete(Motion::Right, &rows));
        assert!(editor.delete(Motion::Left, &rows));
        assert!(!editor.delete(Motion::Left, &rows));
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn option_arrows_move_by_word_and_skip_punctuation() {
        let mut editor = typed("canary at ten, hold—ok");
        go(&mut editor, Motion::WordLeft, false);
        assert_eq!(editor.cursor(), "canary at ten, hold—".len());
        go(&mut editor, Motion::WordLeft, false);
        assert_eq!(editor.cursor(), "canary at ten, ".len());
        go(&mut editor, Motion::DocStart, false);
        go(&mut editor, Motion::WordRight, false);
        assert_eq!(editor.cursor(), "canary".len());
        go(&mut editor, Motion::WordRight, false);
        assert_eq!(editor.cursor(), "canary at".len());
        go(&mut editor, Motion::WordRight, true);
        assert_eq!(editor.selected_text(), " ten");
    }

    #[test]
    fn option_backspace_removes_the_word_before_the_caret() {
        let mut editor = typed("roll back now");
        let rows = rows(&editor);
        assert!(editor.delete(Motion::WordLeft, &rows));
        assert_eq!(editor.text(), "roll back ");
        assert!(editor.delete(Motion::WordLeft, &rows));
        assert_eq!(editor.text(), "roll ");
    }

    #[test]
    fn up_and_down_keep_their_column_across_a_short_line() {
        let mut editor = typed("abcdef\nxy\nuvwxyz");
        go(&mut editor, Motion::DocStart, false);
        for _ in 0..5 {
            go(&mut editor, Motion::Right, false);
        }
        go(&mut editor, Motion::Down, false);
        assert_eq!(editor.cursor(), "abcdef\nxy".len());
        go(&mut editor, Motion::Down, false);
        assert_eq!(editor.cursor(), "abcdef\nxy\nuvwxy".len());
        go(&mut editor, Motion::Up, false);
        go(&mut editor, Motion::Up, false);
        assert_eq!(editor.cursor(), 5);
    }

    #[test]
    fn up_on_the_first_line_goes_to_the_start_and_down_on_the_last_to_the_end() {
        let mut editor = typed("abc\ndef");
        go(&mut editor, Motion::Down, false);
        assert_eq!(editor.cursor(), 7);
        go(&mut editor, Motion::Left, false);
        go(&mut editor, Motion::Up, false);
        assert_eq!(editor.cursor(), 2);
        go(&mut editor, Motion::Up, false);
        assert_eq!(editor.cursor(), 0);
        // Down comes back to the column Up left from.
        go(&mut editor, Motion::Down, true);
        assert_eq!(editor.selected_text(), "abc\nde");
    }

    #[test]
    fn line_keys_stop_at_soft_wraps() {
        let mut editor = typed("abcdefghij\nk");
        let wrapped = PlainRows::new(editor.text(), Some(4));
        assert_eq!(wrapped.count(), 4);
        editor.place(5);
        editor.go(Motion::LineStart, false, &wrapped);
        assert_eq!(editor.cursor(), 4);
        editor.go(Motion::LineEnd, true, &wrapped);
        assert_eq!(editor.selected_text(), "efgh");
        editor.place(1);
        editor.go(Motion::Down, false, &wrapped);
        assert_eq!(editor.cursor(), 5);
        editor.go(Motion::Down, false, &wrapped);
        editor.go(Motion::Down, false, &wrapped);
        assert_eq!(editor.cursor(), 12);
        // Command-Backspace removes back to the start of the visual line.
        editor.place(7);
        assert!(editor.delete(Motion::LineStart, &wrapped));
        assert_eq!(editor.text(), "abcdhij\nk");
    }

    #[test]
    fn shift_extends_and_flips_around_the_anchor() {
        let mut editor = typed("hello");
        editor.place(2);
        go(&mut editor, Motion::Right, true);
        go(&mut editor, Motion::Right, true);
        assert_eq!((editor.selection(), editor.reversed()), (2..4, false));
        for _ in 0..3 {
            go(&mut editor, Motion::Left, true);
        }
        assert_eq!((editor.selection(), editor.reversed()), (1..2, true));
        // A plain arrow collapses to the end it points at.
        go(&mut editor, Motion::Right, false);
        assert_eq!(editor.selection(), 2..2);
        editor.select_all();
        go(&mut editor, Motion::Left, false);
        assert_eq!(editor.selection(), 0..0);
    }

    #[test]
    fn double_click_takes_a_word_and_triple_click_a_line() {
        let mut editor = typed("first line\nsecond one");
        editor.select_word_at(13);
        assert_eq!(editor.selected_text(), "second");
        editor.select_line_at(13);
        assert_eq!(editor.selected_text(), "second one");
        editor.select_line_at(3);
        assert_eq!(editor.selected_text(), "first line");
    }

    #[test]
    fn control_k_cuts_to_the_end_of_the_line_then_the_break() {
        let mut editor = typed("ab\ncd");
        editor.place(1);
        assert!(editor.delete_to_line_end());
        assert_eq!(editor.text(), "a\ncd");
        assert!(editor.delete_to_line_end());
        assert_eq!((editor.text(), editor.selection()), ("acd", 1..1));
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time_and_redo_returns_it() {
        let mut editor = typed("one two");
        assert!(editor.undo());
        assert_eq!(editor.text(), "one ");
        assert!(editor.undo());
        assert_eq!(editor.text(), "");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert!(editor.redo());
        assert_eq!((editor.text(), editor.selection()), ("one two", 7..7));
        assert!(!editor.redo());
        // A new edit after an undo drops what could have been redone.
        editor.undo();
        editor.insert("x");
        assert!(!editor.redo());
        assert_eq!(editor.text(), "one x");
    }

    #[test]
    fn undo_restores_a_deleted_selection_and_a_paste() {
        let mut editor = typed("keep this");
        let rows = rows(&editor);
        editor.place(4);
        editor.go(Motion::DocEnd, true, &rows);
        editor.delete(Motion::Left, &rows);
        assert_eq!(editor.text(), "keep");
        editor.insert(" pasted\ntext");
        assert_eq!(editor.text(), "keep pasted\ntext");
        editor.undo();
        assert_eq!(editor.text(), "keep");
        editor.undo();
        assert_eq!((editor.text(), editor.selection()), ("keep this", 4..9));
        assert_eq!(editor.cut(), Some(" this".to_owned()));
        editor.undo();
        assert_eq!(editor.text(), "keep this");
    }

    #[test]
    fn marked_text_is_replaced_as_the_input_method_settles() {
        let mut editor = typed("a ");
        editor.ime_mark(None, "n", Some(1..1));
        assert_eq!((editor.text(), editor.marked()), ("a n", Some(2..3)));
        editor.ime_mark(None, "ni", Some(2..2));
        assert_eq!((editor.text(), editor.marked(), editor.selection()), ("a ni", Some(2..4), 4..4));
        editor.ime_mark(None, "你", Some(1..1));
        assert_eq!((editor.text(), editor.marked()), ("a 你", Some(2..5)));
        editor.ime_replace(None, "你好");
        assert_eq!((editor.text(), editor.marked(), editor.selection()), ("a 你好", None, 8..8));
        // The whole composition is one undo step.
        editor.undo();
        assert_eq!(editor.text(), "a ");
    }

    #[test]
    fn an_abandoned_composition_leaves_nothing_marked() {
        let mut editor = typed("x");
        editor.ime_mark(None, "ka", None);
        editor.ime_mark(None, "", None);
        assert_eq!((editor.text(), editor.marked(), editor.selection()), ("x", None, 1..1));
    }

    #[test]
    fn the_input_method_speaks_utf16() {
        let mut editor = typed("");
        editor.insert("👍 ok");
        assert_eq!(editor.offset_to_utf16(4), 2);
        assert_eq!(editor.offset_from_utf16(2), 4);
        assert_eq!(editor.range_from_utf16(&(3..5)), 5..7);
        // The emoji picker replaces a UTF-16 range it names itself.
        editor.ime_replace(Some(0..2), "🎉");
        assert_eq!((editor.text(), editor.selection()), ("🎉 ok", 4..4));
        editor.ime_mark(Some(3..5), "かな", Some(0..1));
        assert_eq!((editor.text(), editor.marked(), editor.selection()), ("🎉 かな", Some(5..11), 5..8));
    }

    #[test]
    fn a_new_draft_replaces_the_text_and_cannot_be_undone_into_the_old_one() {
        let mut editor = typed("draft for mara");
        editor.set_text("draft for dev");
        assert_eq!(editor.selection(), 13..13);
        assert!(!editor.undo());
        assert_eq!(editor.text(), "draft for dev");
    }

    #[test]
    fn a_completion_replaces_what_was_typed_and_undo_brings_it_back() {
        let mut editor = typed("ask @de now");
        editor.place(7);
        editor.replace(4..7, "@Dev Patel ");
        assert_eq!((editor.text(), editor.selection()), ("ask @Dev Patel  now", 15..15));
        assert!(editor.undo());
        assert_eq!(editor.text(), "ask @de now");
        // A range past the end is the end.
        editor.replace(8..99, "x");
        assert_eq!(editor.text(), "ask @de x");
    }

    #[test]
    fn offsets_from_the_pointer_land_on_grapheme_boundaries() {
        let mut editor = typed("");
        editor.insert("a👨‍👩‍👧b");
        editor.place(5);
        assert_eq!(editor.selection(), 1..1);
        editor.select_to(999);
        assert_eq!(editor.selection(), 1..editor.text().len());
    }
}
