//! The text field: one line for search, or many for the composer. It began as GPUI's
//! `examples/input.rs` (Zed Industries, Apache-2.0) and keeps its IME handling; the editing itself
//! is `composer::edit`, which has the tests.
//!
//! A multi-line field wraps at its width, grows to `MAX_ROWS` lines and then scrolls inside.
//! Nothing here runs a timer: the caret does not blink, so an idle field costs no frames.

use super::composer::edit::{Editor, Motion, PlainRows, Rows};
use super::theme;
use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, ClipboardEntry, ClipboardItem, Context,
    CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, Font, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent,
    SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use std::ops::Range;
use std::rc::Rc;

/// Lines a multi-line field shows before it scrolls.
pub const MAX_ROWS: usize = 8;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        LineStart,
        LineEnd,
        DocStart,
        DocEnd,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectLineStart,
        SelectLineEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectAll,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        Submit,
        SubmitAlways,
        Newline,
        Escape,
        Tab,
        SubmitAlt,
        PageUp,
        PageDown,
    ]
);

// A grid's own key, bound where the holder says a cell of it is chosen (`PickerGrid`).
actions!(picker, [Star]);

// The composer's own keys, bound where its card is focused.
actions!(composer, [PickEmoji, PickGifs]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(text_bindings());
    // F stars the chosen GIF or sticker; with no cell chosen it is a letter for the search.
    cx.bind_keys([KeyBinding::new("f", Star, Some("PickerGrid"))]);
    let composer = Some("Composer");
    cx.bind_keys([
        KeyBinding::new("secondary-e", PickEmoji, composer),
        KeyBinding::new("secondary-shift-g", PickGifs, composer),
    ]);
}

pub(super) fn text_bindings() -> Vec<KeyBinding> {
    let context = Some("TextInput");
    let bindings = vec![
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-backspace" } else { "ctrl-backspace" }, DeleteWordLeft, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-delete" } else { "ctrl-delete" }, DeleteWordRight, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-backspace" } else { "ctrl-shift-backspace" }, DeleteToLineStart, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-left" } else { "ctrl-left" }, WordLeft, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-right" } else { "ctrl-right" }, WordRight, context),
        KeyBinding::new("home", LineStart, context),
        KeyBinding::new("end", LineEnd, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-up" } else { "ctrl-home" }, DocStart, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-down" } else { "ctrl-end" }, DocEnd, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-shift-left" } else { "ctrl-shift-left" }, SelectWordLeft, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "alt-shift-right" } else { "ctrl-shift-right" }, SelectWordRight, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-shift-up" } else { "ctrl-shift-home" }, SelectDocStart, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-shift-down" } else { "ctrl-shift-end" }, SelectDocEnd, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("secondary-v", Paste, context),
        KeyBinding::new("secondary-c", Copy, context),
        KeyBinding::new("secondary-x", Cut, context),
        KeyBinding::new("secondary-z", Undo, context),
        KeyBinding::new("secondary-shift-z", Redo, context),
        KeyBinding::new(if cfg!(target_os = "macos") { "ctrl-cmd-space" } else { "ctrl-alt-space" }, ShowCharacterPalette, context),
        KeyBinding::new("enter", Submit, context),
        KeyBinding::new("secondary-enter", SubmitAlways, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("alt-enter", SubmitAlt, context),
        KeyBinding::new("escape", Escape, context),
        KeyBinding::new("tab", Tab, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
    ];
    #[cfg(target_os = "macos")]
    let bindings = bindings.into_iter().chain([
        KeyBinding::new("cmd-left", LineStart, context),
        KeyBinding::new("cmd-right", LineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("ctrl-h", Backspace, context),
        KeyBinding::new("ctrl-d", Delete, context),
        KeyBinding::new("ctrl-w", DeleteWordLeft, context),
        KeyBinding::new("ctrl-k", DeleteToLineEnd, context),
        KeyBinding::new("ctrl-b", Left, context),
        KeyBinding::new("ctrl-f", Right, context),
        KeyBinding::new("ctrl-p", Up, context),
        KeyBinding::new("ctrl-n", Down, context),
        KeyBinding::new("ctrl-a", LineStart, context),
        KeyBinding::new("ctrl-e", LineEnd, context),
    ]).collect();
    bindings
}

pub enum InputEvent {
    Changed,
    Submit,
}

/// Option-Return in a field whose holder asked for it (`set_alt_submits`): a silent send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitSilently;

/// Keys the field has no use for itself, for whoever holds it: Escape always; Up and Down in a
/// single-line field, and Up in an empty multi-line one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKey {
    Escape,
    Up,
    Down,
}

/// Keys for a grid the holder draws under a one-line field (`set_grid`): Page Up and Down and
/// Shift-Return always; while a cell is chosen (`set_in_grid`), Left, Right and Command-Up and
/// Down too, which otherwise move the caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputGrid {
    Left,
    Right,
    /// Page Up, Command-Up: the previous section.
    SectionUp,
    /// Page Down, Command-Down: the next section.
    SectionDown,
    /// Shift-Return: choose and keep the grid open.
    Keep,
}

/// Keys for a list the holder has open over the field (`set_list_open`): while it is, the
/// arrows, Return and Tab go to the list and not to the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputList {
    Up,
    Down,
    Complete,
}

/// A paste that was not text, for the holder to attach. The field takes nothing from it.
#[derive(Debug, Clone, PartialEq)]
pub enum InputPaste {
    Image(gpui::Image),
    Files(Vec<std::path::PathBuf>),
}

impl EventEmitter<InputEvent> for TextInput {}
impl EventEmitter<InputKey> for TextInput {}
impl EventEmitter<InputList> for TextInput {}
impl EventEmitter<InputGrid> for TextInput {}
impl EventEmitter<InputPaste> for TextInput {}
impl EventEmitter<SubmitSilently> for TextInput {}

/// One visual line of the field as last drawn.
struct Row {
    range: Range<usize>,
    /// The hard line it is a piece of, and where that line starts in the text.
    line: usize,
    line_start: usize,
    /// Where this row begins inside the unwrapped line.
    start_x: Pixels,
    /// Soft-wrapped: the next row continues the same hard line.
    wraps: bool,
}

/// The field's text as shaped for one width.
struct Laid {
    lines: Vec<WrappedLine>,
    rows: Vec<Row>,
    text: SharedString,
}

impl Laid {
    fn x(&self, row: &Row, offset: usize) -> Pixels {
        let layout = &self.lines[row.line].unwrapped_layout;
        layout.x_for_index(offset.clamp(row.range.start, row.range.end) - row.line_start) - row.start_x
    }
}

impl Rows for Laid {
    fn count(&self) -> usize {
        self.rows.len()
    }

    fn range(&self, row: usize) -> Range<usize> {
        self.rows[row].range.clone()
    }

    fn x_of(&self, row: usize, offset: usize) -> f32 {
        self.x(&self.rows[row], offset).into()
    }

    fn offset_at(&self, row: usize, x: f32) -> usize {
        let row = &self.rows[row];
        let layout = &self.lines[row.line].unwrapped_layout;
        let found = row.line_start + layout.closest_index_for_x(row.start_x + px(x.max(0.)));
        // The end of a wrapped row is the start of the next; stay on this one.
        let end = if row.wraps {
            self.text[row.range.clone()]
                .char_indices()
                .next_back()
                .map_or(row.range.start, |(ix, _)| row.range.start + ix)
        } else {
            row.range.end
        };
        found.clamp(row.range.start, end.max(row.range.start))
    }
}

/// Byte ranges of the markup characters in one line, which stay visible but step back.
fn markup(line: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if trimmed.starts_with("```") {
        found.push(indent..indent + 3);
        return found;
    }
    for lead in ["> ", "- ", "* "] {
        if trimmed.starts_with(lead) {
            found.push(indent..indent + 1);
        }
    }
    let bytes = line.as_bytes();
    let mut at = found.last().map_or(0, |range: &Range<usize>| range.end);
    while at < bytes.len() {
        let marker = [("**", 2), ("~~", 2), ("`", 1), ("_", 1), ("*", 1)]
            .into_iter()
            .find(|(marker, _)| line[at..].starts_with(marker));
        let Some((marker, len)) = marker else {
            at += line[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        // A marker opens only against text, and needs its partner later in the line.
        let inside = &line[at + len..];
        let opens = inside.chars().next().is_some_and(|ch| !ch.is_whitespace())
            && (marker != "_" || line[..at].chars().next_back().is_none_or(|ch| !ch.is_alphanumeric()));
        match inside.find(marker).filter(|close| opens && *close > 0) {
            Some(close) => {
                found.push(at..at + len);
                found.push(at + len + close..at + 2 * len + close);
                at += 2 * len + close;
            }
            None => at += len,
        }
    }
    found
}

pub struct TextInput {
    focus_handle: FocusHandle,
    editor: Editor,
    placeholder: SharedString,
    laid: Option<Rc<Laid>>,
    bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    /// How far the text is scrolled: down in a multi-line field, sideways in a single-line one.
    scroll: Pixels,
    is_selecting: bool,
    list_open: bool,
    alt_submits: bool,
    grid: bool,
    in_grid: bool,
    /// Ranges drawn as mentions. The holder keeps them in step with the text.
    highlights: Vec<Range<usize>>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> TextInput {
        TextInput::with(false, placeholder, cx)
    }

    /// Return sends, Shift-Return breaks the line.
    pub fn multiline(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> TextInput {
        TextInput::with(true, placeholder, cx)
    }

    fn with(multiline: bool, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> TextInput {
        TextInput {
            focus_handle: cx.focus_handle(),
            editor: Editor::new(multiline),
            placeholder: placeholder.into(),
            laid: None,
            bounds: None,
            line_height: px(20.),
            scroll: px(0.),
            is_selecting: false,
            list_open: false,
            alt_submits: false,
            grid: false,
            in_grid: false,
            highlights: Vec::new(),
        }
    }

    pub fn text(&self) -> &str {
        self.editor.text()
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>) {
        self.placeholder = placeholder.into();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
        cx.emit(InputEvent::Changed);
    }

    /// Puts another draft in the field, caret at its end. Not an edit: nothing is emitted and
    /// undo does not lead back to the text that was here.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.editor.set_text(text);
        self.laid = None;
        self.scroll = px(0.);
        self.is_selecting = false;
        cx.notify();
    }

    pub fn cursor(&self) -> usize {
        self.editor.cursor()
    }

    pub fn set_list_open(&mut self, open: bool) {
        self.list_open = open;
    }

    /// The holder draws a grid under the field and takes its keys (`InputGrid`).
    pub fn set_grid(&mut self, on: bool) {
        self.grid = on;
    }

    /// A cell of the holder's grid is chosen: the arrows sideways move it, not the caret.
    pub fn set_in_grid(&mut self, on: bool) {
        self.in_grid = on;
    }

    /// Option-Return sends silently rather than breaking the line.
    pub fn set_alt_submits(&mut self, on: bool) {
        self.alt_submits = on;
    }

    pub fn set_highlights(&mut self, highlights: Vec<Range<usize>>, cx: &mut Context<Self>) {
        if self.highlights != highlights {
            self.highlights = highlights;
            cx.notify();
        }
    }

    /// Types `text` at the caret, as a key would.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.editor.insert(text);
        self.edited(cx);
    }

    /// Puts `text` in place of `range`, caret after it: one step for undo.
    pub fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.editor.replace(range, text);
        self.edited(cx);
    }

    fn rows(&self) -> Rc<dyn Rows> {
        match &self.laid {
            Some(laid) if laid.text.as_ref() == self.editor.text() => laid.clone(),
            _ => Rc::new(PlainRows::new(self.editor.text(), None)),
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn go(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        let rows = self.rows();
        self.editor.go(motion, extend, rows.as_ref());
        if extend { self.publish_primary(cx); }
        cx.notify();
    }

    fn remove(&mut self, motion: Motion, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows();
        if self.editor.delete(motion, rows.as_ref()) {
            self.edited(cx);
        } else {
            window.play_system_bell();
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Motion::Left, window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Motion::Right, window, cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Motion::WordLeft, window, cx);
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Motion::WordRight, window, cx);
    }

    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Motion::LineStart, window, cx);
    }

    fn delete_to_line_end(&mut self, _: &DeleteToLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.delete_to_line_end() {
            self.edited(cx);
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid && self.in_grid {
            true => cx.emit(InputGrid::Left),
            false => self.go(Motion::Left, false, cx),
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid && self.in_grid {
            true => cx.emit(InputGrid::Right),
            false => self.go(Motion::Right, false, cx),
        }
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid {
            true => cx.emit(InputGrid::SectionUp),
            false => cx.propagate(),
        }
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid {
            true => cx.emit(InputGrid::SectionDown),
            false => cx.propagate(),
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.list_open {
            cx.emit(InputList::Up);
        } else if !self.editor.is_multiline() || self.editor.text().is_empty() {
            cx.emit(InputKey::Up);
        } else {
            self.go(Motion::Up, false, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.list_open {
            cx.emit(InputList::Down);
        } else if self.editor.is_multiline() {
            self.go(Motion::Down, false, cx);
        } else {
            cx.emit(InputKey::Down);
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::WordLeft, false, cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::WordRight, false, cx);
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::LineStart, false, cx);
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::LineEnd, false, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid && self.in_grid {
            true => cx.emit(InputGrid::SectionUp),
            false => self.go(Motion::DocStart, false, cx),
        }
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        match self.grid && self.in_grid {
            true => cx.emit(InputGrid::SectionDown),
            false => self.go(Motion::DocEnd, false, cx),
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::Left, true, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::Right, true, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::Up, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::Down, true, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::WordLeft, true, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::WordRight, true, cx);
    }

    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::LineStart, true, cx);
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::LineEnd, true, cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::DocStart, true, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Motion::DocEnd, true, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.select_all();
        self.publish_primary(cx);
        cx.notify();
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        // Return during composition belongs to the input method.
        if self.editor.marked().is_some() {
            return;
        }
        if self.list_open {
            cx.emit(InputList::Complete);
        } else if self.editor.is_multiline() && super::app::prefs::choices().command_return {
            // Settings › Messages › Send with ⌘Return (M1): Return adds a line.
            self.newline(&Newline, window, cx);
        } else {
            cx.emit(InputEvent::Submit);
        }
    }

    /// Tab completes from an open list. With none, a multi-line field takes nothing from it;
    /// a single-line field leaves the key to whoever holds it.
    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.list_open {
            cx.emit(InputList::Complete);
        } else if !self.editor.is_multiline() {
            cx.propagate();
        }
    }

    fn submit_always(&mut self, _: &SubmitAlways, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.unmark();
        cx.emit(InputEvent::Submit);
    }

    fn submit_alt(&mut self, _: &SubmitAlt, window: &mut Window, cx: &mut Context<Self>) {
        if !self.alt_submits || !self.editor.is_multiline() {
            self.newline(&Newline, window, cx);
        } else if self.editor.marked().is_none() {
            cx.emit(SubmitSilently);
        }
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.grid && !self.editor.is_multiline() {
            if self.editor.marked().is_none() {
                cx.emit(InputGrid::Keep);
            }
        } else if self.editor.newline() {
            self.edited(cx);
        } else if self.editor.marked().is_none() {
            cx.emit(InputEvent::Submit);
        }
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputKey::Escape);
    }

    fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.undo() {
            self.edited(cx);
        } else {
            window.play_system_bell();
        }
    }

    fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.redo() {
            self.edited(cx);
        } else {
            window.play_system_bell();
        }
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    /// Finder files first, then a picture that came with no text, else text: a browser's copy
    /// carries both a picture and its words, and stays words.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        match pasted(item.entries()) {
            Pasted::Files(paths) => cx.emit(InputPaste::Files(paths)),
            Pasted::Image(image) => cx.emit(InputPaste::Image(image)),
            Pasted::Text => {
                if let Some(text) = item.text() {
                    self.editor.insert(&text);
                    self.edited(cx);
                }
            }
            Pasted::Nothing => {}
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.selection().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.editor.selected_text().to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(cut) = self.editor.cut() {
            cx.write_to_clipboard(ClipboardItem::new_string(cut));
            self.edited(cx);
        }
    }

    /// Linux PRIMARY is selected text, separate from the Copy/Paste clipboard.
    fn publish_primary(&self, _cx: &App) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if !self.editor.selection().is_empty() {
            _cx.write_to_primary(ClipboardItem::new_string(self.editor.selected_text().to_owned()));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn on_middle_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Read before moving the caret: the selected text may belong to this field.
        let text = cx.read_from_primary().and_then(|item| item.text());
        let offset = self.index_for_mouse_position(event.position);
        window.focus(&self.focus_handle(cx), cx);
        self.is_selecting = false;
        if paste_primary_text(&mut self.editor, text.as_deref(), offset) {
            self.edited(cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let offset = self.index_for_mouse_position(event.position);
        self.is_selecting = select_on_press(&mut self.editor, offset, event.modifiers.shift, event.click_count);
        if !self.is_selecting { self.publish_primary(cx); }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.is_selecting, false) {
            self.publish_primary(cx);
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.editor.select_to(self.index_for_mouse_position(event.position));
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(laid), Some(bounds)) = (&self.laid, self.bounds) else {
            return;
        };
        if !self.editor.is_multiline() {
            return;
        }
        let most = (self.line_height * laid.rows.len() as f32 - bounds.size.height).max(px(0.));
        let next = (self.scroll - event.delta.pixel_delta(self.line_height).y).clamp(px(0.), most);
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(laid)) = (self.bounds, self.laid.as_ref()) else {
            return 0;
        };
        if self.editor.text().is_empty() || laid.text.as_ref() != self.editor.text() {
            return self.editor.text().len().min(self.editor.cursor());
        }
        let (scroll_x, scroll_y) = self.scrolled();
        let y = position.y - bounds.top() + scroll_y;
        if y < px(0.) {
            return 0;
        }
        let row = (y / self.line_height) as usize;
        if row >= laid.rows.len() {
            return self.editor.text().len();
        }
        laid.offset_at(row, (position.x - bounds.left() + scroll_x).into())
    }

    fn scrolled(&self) -> (Pixels, Pixels) {
        if self.editor.is_multiline() {
            (px(0.), self.scroll)
        } else {
            (self.scroll, px(0.))
        }
    }
}

#[derive(Debug, PartialEq)]
enum Pasted {
    Files(Vec<std::path::PathBuf>),
    Image(gpui::Image),
    Text,
    Nothing,
}

/// Only a plain click starts a drag. Word, line and shift selections are complete
/// on press and must survive a move event at the same pointer position.
fn select_on_press(editor: &mut Editor, offset: usize, shift: bool, clicks: usize) -> bool {
    match (shift, clicks) {
        (true, _) => editor.select_to(offset),
        (false, 2) => editor.select_word_at(offset),
        (false, 3..) => editor.select_line_at(offset),
        (false, _) => { editor.place(offset); return true; }
    }
    false
}

#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
fn paste_primary_text(editor: &mut Editor, text: Option<&str>, offset: usize) -> bool {
    let Some(text) = text.filter(|text| !text.is_empty()) else { return false; };
    editor.place(offset);
    editor.insert(text);
    true
}

fn pasted(entries: &[ClipboardEntry]) -> Pasted {
    let paths: Vec<std::path::PathBuf> = entries
        .iter()
        .filter_map(|entry| match entry {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
            _ => None,
        })
        .flatten()
        .collect();
    if !paths.is_empty() {
        return Pasted::Files(paths);
    }
    let text = entries.iter().any(|entry| matches!(entry, ClipboardEntry::String(_)));
    let image = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image.clone()),
        _ => None,
    });
    match (text, image) {
        (true, _) => Pasted::Text,
        (false, Some(image)) => Pasted::Image(image),
        (false, None) => Pasted::Nothing,
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.editor.range_from_utf16(&range_utf16);
        actual_range.replace(self.editor.range_to_utf16(&range));
        Some(self.editor.text()[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.editor.range_to_utf16(&self.editor.selection()),
            reversed: self.editor.reversed(),
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.editor.marked().map(|range| self.editor.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.editor.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.ime_replace(range_utf16, new_text);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.ime_mark(range_utf16, new_text, new_selected_range_utf16);
        self.edited(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let laid = self.laid.as_ref()?;
        let range = self.editor.range_from_utf16(&range_utf16);
        let index = laid.row_of(range.start);
        let row = laid.rows.get(index)?;
        let (scroll_x, scroll_y) = self.scrolled();
        let top = bounds.top() + self.line_height * index as f32 - scroll_y;
        Some(Bounds::from_corners(
            point(bounds.left() + laid.x(row, range.start) - scroll_x, top),
            point(bounds.left() + laid.x(row, range.end) - scroll_x, top + self.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.bounds?.contains(&point).then_some(())?;
        Some(self.editor.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

/// What shaping needs from the text style in force where the field is drawn.
#[derive(Clone)]
struct Shaping {
    font: Font,
    font_size: Pixels,
    color: Hsla,
}

impl Shaping {
    /// Shapes each hard line by itself, so a run never has to be split across a line break.
    fn lay_out(&self, input: &TextInput, wrap: Option<Pixels>, window: &mut Window) -> Laid {
        let content = input.editor.text();
        let (text, placeholder): (SharedString, bool) = if content.is_empty() {
            // A placeholder never wraps or breaks.
            (input.placeholder.replace('\n', " ").into(), true)
        } else {
            (SharedString::new(content), false)
        };
        let marked = input.editor.marked().filter(|_| !placeholder);
        let run = |len: usize, color: Hsla, underlined: bool| TextRun {
            len,
            font: self.font.clone(),
            color,
            background_color: None,
            underline: underlined.then_some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: None,
        };
        let faint: Hsla = theme::text_faint().into();
        let mention: Hsla = theme::accent_text().into();
        let mut lines = Vec::new();
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in text.split('\n') {
            // Cut points where the color or the underline changes.
            let dim = if placeholder || !input.editor.is_multiline() { Vec::new() } else { markup(line) };
            let marked = marked.as_ref().map(|marked| {
                marked.start.saturating_sub(line_start).min(line.len())
                    ..marked.end.saturating_sub(line_start).min(line.len())
            });
            let within = |range: &Range<usize>| {
                range.start.saturating_sub(line_start).min(line.len())..range.end.saturating_sub(line_start).min(line.len())
            };
            let named: Vec<Range<usize>> = if placeholder { Vec::new() } else { input.highlights.iter().map(within).collect() };
            let mut cuts = vec![0, line.len()];
            cuts.extend(dim.iter().flat_map(|range| [range.start, range.end]));
            cuts.extend(named.iter().flat_map(|range| [range.start, range.end]).filter(|cut| line.is_char_boundary(*cut)));
            cuts.extend(marked.iter().flat_map(|range| [range.start, range.end]));
            cuts.sort_unstable();
            cuts.dedup();
            let runs: Vec<TextRun> = cuts
                .windows(2)
                .map(|cut| {
                    let dimmed = placeholder || dim.iter().any(|range| range.start <= cut[0] && cut[1] <= range.end);
                    let underlined = marked.as_ref().is_some_and(|range| range.start <= cut[0] && cut[1] <= range.end);
                    let named = named.iter().any(|range| range.start <= cut[0] && cut[1] <= range.end);
                    let color = match (dimmed, named) {
                        (true, _) => faint,
                        (false, true) => mention,
                        (false, false) => self.color,
                    };
                    run(cut[1] - cut[0], color, underlined)
                })
                .collect();
            let wrap = wrap.filter(|_| !placeholder);
            let shaped = window
                .text_system()
                .shape_text(SharedString::new(line), self.font_size, &runs, wrap, None)
                .ok()
                .and_then(|mut shaped| shaped.pop())
                .unwrap_or_default();
            let layout = &shaped.unwrapped_layout;
            let mut row_start = (0, px(0.));
            for boundary in shaped.wrap_boundaries.iter() {
                let glyph = &layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                rows.push(Row {
                    range: line_start + row_start.0..line_start + glyph.index,
                    line: lines.len(),
                    line_start,
                    start_x: row_start.1,
                    wraps: true,
                });
                row_start = (glyph.index, glyph.position.x);
            }
            rows.push(Row {
                range: line_start + row_start.0..line_start + line.len(),
                line: lines.len(),
                line_start,
                start_x: row_start.1,
                wraps: false,
            });
            lines.push(shaped);
            line_start += line.len() + 1;
        }
        Laid { lines, rows, text }
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    laid: Rc<Laid>,
    shaping: Shaping,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    thumb: Option<PaintQuad>,
    scroll: (Pixels, Pixels),
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = Shaping;
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();
        let shaping = Shaping {
            font: text_style.font(),
            font_size: text_style.font_size.to_pixels(window.rem_size()),
            color: text_style.color,
        };
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if !self.input.read(cx).editor.is_multiline() {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), shaping);
        }
        // The height follows the text at the width the field is given.
        let input = self.input.clone();
        let measuring = shaping.clone();
        let layout = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows = match width {
                Some(width) => measuring.lay_out(input.read(cx), Some(width), window).rows.len(),
                None => 1,
            };
            size(width.unwrap_or_default(), line_height * rows.clamp(1, MAX_ROWS) as f32)
        });
        (layout, shaping)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        shaping: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let line_height = window.line_height();
        let input = self.input.read(cx);
        let multiline = input.editor.is_multiline();
        let laid = Rc::new(shaping.lay_out(input, multiline.then_some(bounds.size.width), window));
        let empty = input.editor.text().is_empty();
        let selection = if empty { 0..0 } else { input.editor.selection() };
        let caret = if empty { 0 } else { input.editor.cursor() };
        let caret_row = laid.row_of(caret);
        let caret_x = laid.x(&laid.rows[caret_row], caret);

        // Keep the caret's line in view, and never scroll past the text.
        let mut scroll = input.scroll;
        if multiline {
            let top = line_height * caret_row as f32;
            let most = (line_height * laid.rows.len() as f32 - bounds.size.height).max(px(0.));
            if !input.is_selecting || top < scroll || top + line_height > scroll + bounds.size.height {
                scroll = scroll.clamp((top + line_height - bounds.size.height).max(px(0.)), top);
            }
            scroll = scroll.clamp(px(0.), most);
        } else {
            let room = bounds.size.width - px(2.);
            scroll = scroll.clamp((caret_x - room).max(px(0.)), caret_x);
        }
        let (scroll_x, scroll_y) = if multiline { (px(0.), scroll) } else { (scroll, px(0.)) };
        let origin = point(bounds.left() - scroll_x, bounds.top() - scroll_y);

        let cursor = selection.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(origin.x + caret_x, origin.y + line_height * caret_row as f32),
                    size(px(2.), line_height),
                ),
                theme::accent_text(),
            )
        });
        let selected = laid
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| selection.start <= row.range.end && row.range.start < selection.end)
            .filter(|_| !selection.is_empty())
            .map(|(ix, row)| {
                let from = laid.x(row, selection.start.max(row.range.start));
                // A selected line break shows as a sliver past the line's end.
                let past = if selection.end > row.range.end { px(6.) } else { px(0.) };
                let to = laid.x(row, selection.end.min(row.range.end)) + past;
                fill(
                    Bounds::from_corners(
                        point(origin.x + from, origin.y + line_height * ix as f32),
                        point(origin.x + to, origin.y + line_height * (ix + 1) as f32),
                    ),
                    theme::selection(),
                )
            })
            .collect();
        let total = line_height * laid.rows.len() as f32;
        let thumb = (multiline && total > bounds.size.height).then(|| {
            let length = (bounds.size.height * (bounds.size.height / total)).max(px(16.));
            let travel = bounds.size.height - length;
            let at = travel * (scroll_y / (total - bounds.size.height));
            fill(
                Bounds::new(point(bounds.right() - px(4.), bounds.top() + at), size(px(4.), length)),
                theme::selected(),
            )
            .corner_radii(px(2.))
        });
        PrepaintState {
            laid,
            shaping: shaping.clone(),
            cursor,
            selection: selected,
            thumb,
            scroll: (scroll_x, scroll_y),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let line_height = window.line_height();
        let (scroll_x, scroll_y) = prepaint.scroll;
        let laid = prepaint.laid.clone();
        let focused = focus_handle.is_focused(window);
        let _ = &prepaint.shaping;
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selection in prepaint.selection.drain(..) {
                window.paint_quad(selection);
            }
            let mut row = 0;
            for line in &laid.lines {
                let origin = point(bounds.left() - scroll_x, bounds.top() - scroll_y + line_height * row as f32);
                let rows = line.wrap_boundaries.len() + 1;
                let visible = origin.y < bounds.bottom() && origin.y + line_height * rows as f32 > bounds.top();
                if visible {
                    let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
                row += rows;
            }
            if let Some(cursor) = prepaint.cursor.take().filter(|_| focused) {
                window.paint_quad(cursor);
            }
            if let Some(thumb) = prepaint.thumb.take() {
                window.paint_quad(thumb);
            }
        });

        self.input.update(cx, |input, _| {
            input.laid = Some(laid);
            input.bounds = Some(bounds);
            input.line_height = line_height;
            input.scroll = if input.editor.is_multiline() { scroll_y } else { scroll_x };
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::submit_always))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit_alt))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::tab))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .map(|input| {
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                let input = input.on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_down));
                input
            })
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{markup, pasted, Pasted};
    use gpui::{ClipboardEntry, ClipboardString, ExternalPaths, Image, ImageFormat};
    use std::path::PathBuf;

    #[test]
    fn word_line_and_shift_selections_survive_a_stationary_move() {
        for (clicks, shift, expected) in [(2, false, "second"), (3, false, "first second third"), (1, true, "second thi")] {
            let mut editor = super::Editor::new(true);
            editor.set_text("first second third");
            editor.place(16);
            let dragging = super::select_on_press(&mut editor, 6, shift, clicks);
            if dragging { editor.select_to(6); }
            assert!(!dragging);
            assert_eq!(editor.selected_text(), expected);
        }
        let mut editor = super::Editor::new(true);
        editor.set_text("first second");
        assert!(super::select_on_press(&mut editor, 6, false, 1));
        editor.select_to(12);
        assert_eq!(editor.selected_text(), "second");
    }

    #[test]
    fn middle_paste_inserts_at_the_click_without_replacing_selected_text() {
        let mut editor = super::Editor::new(true);
        editor.set_text("hello 世界");
        editor.select_all();
        assert!(super::paste_primary_text(&mut editor, Some("https://example.invalid "), 6));
        assert_eq!(editor.text(), "hello https://example.invalid 世界");
        assert!(editor.selection().is_empty());
        editor.undo();
        assert_eq!(editor.text(), "hello 世界");
    }

    #[test]
    fn an_empty_primary_selection_does_not_move_or_replace_the_draft() {
        let mut editor = super::Editor::new(true);
        editor.set_text("draft");
        editor.select_all();
        for text in [None, Some("")] {
            assert!(!super::paste_primary_text(&mut editor, text, 2));
            assert_eq!(editor.text(), "draft");
            assert_eq!(editor.selected_text(), "draft");
        }
        let mut search = super::Editor::new(false);
        assert!(super::paste_primary_text(&mut search, Some("one\r\ntwo"), 0));
        assert_eq!(search.text(), "one two");
    }

    #[test]
    fn finder_files_come_first_then_a_picture_without_words_else_words() {
        let words = ClipboardEntry::String(ClipboardString::new("trail.pdf".into()));
        let files = ClipboardEntry::ExternalPaths(ExternalPaths([PathBuf::from("/Users/me/trail.pdf")].into_iter().collect()));
        let picture = ClipboardEntry::Image(Image::from_bytes(ImageFormat::Png, vec![1, 2, 3]));
        assert_eq!(pasted(&[words.clone(), files]), Pasted::Files(vec![PathBuf::from("/Users/me/trail.pdf")]));
        assert!(matches!(pasted(std::slice::from_ref(&picture)), Pasted::Image(image) if image.format == ImageFormat::Png));
        assert_eq!(pasted(&[picture, words.clone()]), Pasted::Text, "a browser's copy stays words");
        assert_eq!(pasted(&[words]), Pasted::Text);
        assert_eq!(pasted(&[]), Pasted::Nothing);
    }

    fn marked(line: &str) -> Vec<&str> {
        markup(line).into_iter().map(|range| &line[range]).collect()
    }

    #[test]
    fn markup_characters_are_found_only_in_pairs() {
        assert_eq!(marked("hold **20 minutes** now"), ["**", "**"]);
        assert_eq!(marked("under `0.1%` and ~~not~~ _really_"), ["`", "`", "~~", "~~", "_", "_"]);
        assert_eq!(marked("2 * 3 and snake_case_name and a lone ` tick"), Vec::<&str>::new());
        assert_eq!(marked("- canary at 22:00"), ["-"]);
        assert_eq!(marked("  > quoted *text*"), [">", "*", "*"]);
        assert_eq!(marked("```rust"), ["```"]);
        assert_eq!(marked("naïve **café** 👍"), ["**", "**"]);
    }
}
