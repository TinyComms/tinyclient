//! The reaction picker: a search field over one virtual grid. Glyphs come from the system's
//! emoji font through the text system, so there is no sprite sheet and no image set; the
//! conversation's custom emoji are decoded only while their row is on screen. The picker is
//! made when it opens and dropped when it closes.

use super::images::{Images, Loaded, Picture};
use super::media;
use crate::gui::components::field;
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::theme::{self, Type};
use gpui::{
    div, img, prelude::*, px, uniform_list, Context, Entity, EventEmitter, FocusHandle, Focusable, ImageSource,
    SharedString, Subscription, Window,
};
use protocol::MediaFile;
use std::cell::RefCell;
use std::ops::Range;

pub const WIDTH: f32 = 352.;
const COLUMNS: usize = 9;
const CELL: f32 = 36.;
const LABEL: f32 = 28.;
/// Rows of the grid shown at once.
const ROWS: f32 = 6.;
/// Recents kept: two rows.
const RECENT: usize = 18;

/// The glyph, then the words it is found by. A small set: what people react with.
const CATALOG: &[(&str, &str)] = &[
    ("👍", "thumbs up yes +1 like agree"),
    ("👎", "thumbs down no -1 dislike"),
    ("❤️", "heart love red"),
    ("😂", "joy laugh tears lol"),
    ("🎉", "tada party celebrate hooray"),
    ("🙏", "pray thanks please"),
    ("👀", "eyes look see"),
    ("🔥", "fire hot lit"),
    ("✅", "check done yes ok"),
    ("🚀", "rocket ship launch"),
    ("😀", "grinning smile happy"),
    ("😃", "smiley happy"),
    ("😄", "smile happy"),
    ("😁", "grin beam"),
    ("😆", "laughing satisfied"),
    ("😅", "sweat smile phew"),
    ("🤣", "rofl rolling laugh"),
    ("🙂", "slight smile"),
    ("🙃", "upside down"),
    ("😉", "wink"),
    ("😊", "blush smile"),
    ("😇", "innocent angel halo"),
    ("🥰", "smiling hearts love"),
    ("😍", "heart eyes love"),
    ("🤩", "star struck wow"),
    ("😘", "kiss"),
    ("😋", "yum tasty"),
    ("😛", "tongue"),
    ("😜", "wink tongue"),
    ("🤪", "zany crazy"),
    ("🤗", "hug"),
    ("🤭", "oops giggle"),
    ("🤫", "shush quiet"),
    ("🤔", "thinking hmm"),
    ("🤐", "zipper mouth"),
    ("🤨", "raised eyebrow skeptical"),
    ("😐", "neutral"),
    ("😑", "expressionless"),
    ("😶", "no mouth"),
    ("😏", "smirk"),
    ("😒", "unamused"),
    ("🙄", "eye roll"),
    ("😬", "grimace awkward"),
    ("😌", "relieved"),
    ("😔", "pensive sad"),
    ("😴", "sleeping zzz"),
    ("🤒", "sick thermometer"),
    ("🤯", "mind blown exploding"),
    ("🥳", "partying celebrate"),
    ("😎", "cool sunglasses"),
    ("🤓", "nerd"),
    ("😕", "confused"),
    ("😟", "worried"),
    ("🙁", "frown"),
    ("😮", "open mouth wow"),
    ("😲", "astonished"),
    ("😳", "flushed"),
    ("🥺", "pleading"),
    ("😢", "cry sad tear"),
    ("😭", "sob crying"),
    ("😱", "scream shock"),
    ("😤", "triumph huff"),
    ("😡", "rage angry"),
    ("🤬", "cursing swear"),
    ("💀", "skull dead"),
    ("💩", "poop"),
    ("🤡", "clown"),
    ("👻", "ghost"),
    ("🤖", "robot bot"),
    ("👋", "wave hello bye"),
    ("🤚", "raised back hand"),
    ("✋", "raised hand stop high five"),
    ("👌", "ok perfect"),
    ("🤌", "pinched fingers"),
    ("✌️", "victory peace"),
    ("🤞", "crossed fingers luck"),
    ("🤟", "love you"),
    ("🤘", "rock horns"),
    ("👈", "point left"),
    ("👉", "point right"),
    ("👆", "point up"),
    ("👇", "point down"),
    ("☝️", "index up one"),
    ("✊", "fist"),
    ("👊", "punch fist bump"),
    ("👏", "clap applause"),
    ("🙌", "raised hands praise hooray"),
    ("🫶", "heart hands"),
    ("🤝", "handshake deal"),
    ("💪", "muscle strong flex"),
    ("🫡", "salute"),
    ("🧠", "brain"),
    ("👁️", "eye"),
    ("🧡", "orange heart"),
    ("💛", "yellow heart"),
    ("💚", "green heart"),
    ("💙", "blue heart"),
    ("💜", "purple heart"),
    ("🖤", "black heart"),
    ("🤍", "white heart"),
    ("💔", "broken heart"),
    ("💯", "hundred 100 perfect"),
    ("💥", "boom collision"),
    ("💫", "dizzy star"),
    ("✨", "sparkles shiny"),
    ("⭐", "star"),
    ("🌟", "glowing star"),
    ("⚡", "zap lightning fast"),
    ("☀️", "sun sunny"),
    ("🌙", "moon night"),
    ("🌈", "rainbow"),
    ("☁️", "cloud"),
    ("❄️", "snowflake cold"),
    ("🌊", "wave water ocean"),
    ("🌱", "seedling sprout"),
    ("🌿", "herb leaf plant"),
    ("🍀", "clover luck"),
    ("🌲", "tree evergreen"),
    ("🍄", "mushroom fungi"),
    ("🌸", "blossom flower"),
    ("🐶", "dog"),
    ("🐱", "cat"),
    ("🦊", "fox"),
    ("🐻", "bear"),
    ("🐼", "panda"),
    ("🐸", "frog"),
    ("🐵", "monkey"),
    ("🦄", "unicorn"),
    ("🐝", "bee"),
    ("🦋", "butterfly"),
    ("🐢", "turtle slow"),
    ("🦀", "crab rust"),
    ("🐙", "octopus"),
    ("🍎", "apple"),
    ("🍕", "pizza"),
    ("🍔", "burger"),
    ("🌮", "taco"),
    ("🍪", "cookie"),
    ("🎂", "cake birthday"),
    ("🍿", "popcorn"),
    ("☕", "coffee"),
    ("🍵", "tea"),
    ("🍺", "beer"),
    ("🍷", "wine"),
    ("🥂", "cheers toast"),
    ("⚽", "soccer football"),
    ("🏆", "trophy win"),
    ("🥇", "gold medal first"),
    ("🎯", "target bullseye dart"),
    ("🎮", "game controller"),
    ("🎲", "dice"),
    ("🎵", "music note"),
    ("🎸", "guitar"),
    ("🎬", "clapper movie"),
    ("📷", "camera photo"),
    ("💡", "bulb idea"),
    ("🔔", "bell"),
    ("📌", "pin pushpin"),
    ("📎", "paperclip"),
    ("🔒", "lock"),
    ("🔑", "key"),
    ("🔧", "wrench fix tool"),
    ("🔨", "hammer"),
    ("🐛", "bug"),
    ("💻", "laptop computer"),
    ("📱", "phone"),
    ("📈", "chart up growth"),
    ("📉", "chart down"),
    ("📝", "memo note write"),
    ("📚", "books"),
    ("📦", "package box ship"),
    ("💰", "money bag"),
    ("⏰", "alarm clock time"),
    ("⏳", "hourglass wait"),
    ("🚧", "construction wip"),
    ("🚨", "siren alert"),
    ("⚠️", "warning"),
    ("❌", "x cross no"),
    ("❓", "question"),
    ("❗", "exclamation"),
    ("➕", "plus"),
    ("➖", "minus"),
    ("🆗", "ok"),
    ("🏁", "finish flag"),
    ("🏳️‍🌈", "rainbow flag pride"),
];

thread_local! {
    /// What was reacted with, newest first. Kept for the life of the window.
    // CONTRACT: recents are per account in the spec; nothing stores them, so they are the
    // window's and start over at each launch.
    static RECENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

const DEFAULTS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "🙏", "👀"];

/// The `count` emoji used most recently, filled out with the defaults.
pub fn recents(count: usize) -> Vec<String> {
    RECENTS.with_borrow(|used| filled(used, count))
}

fn filled(used: &[String], count: usize) -> Vec<String> {
    let mut out: Vec<String> = used.iter().take(count).cloned().collect();
    for glyph in DEFAULTS {
        if out.len() >= count {
            break;
        }
        if !out.iter().any(|held| held == glyph) {
            out.push(glyph.to_owned());
        }
    }
    out
}

pub fn used(emoji: &str) {
    RECENTS.with_borrow_mut(|recent| bump(recent, emoji));
}

fn bump(recent: &mut Vec<String>, emoji: &str) {
    recent.retain(|held| held != emoji);
    recent.insert(0, emoji.to_owned());
    recent.truncate(RECENT);
}

/// One cell of the grid.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Glyph(&'static str),
    /// Used before, and not from the catalog's static text.
    Recent(String),
    Custom { name: String, file: MediaFile },
}

impl Cell {
    /// What `React` is sent: the emoji itself, or a custom one's `:name:`.
    pub fn emoji(&self) -> String {
        match self {
            Cell::Glyph(glyph) => (*glyph).to_owned(),
            Cell::Recent(emoji) => emoji.clone(),
            Cell::Custom { name, .. } => name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Label(&'static str),
    Cells(Vec<Cell>),
}

fn chunked(rows: &mut Vec<Row>, cells: Vec<Cell>) {
    for chunk in cells.chunks(COLUMNS) {
        rows.push(Row::Cells(chunk.to_vec()));
    }
}

/// The grid for a query. Empty: recents, the conversation's own emoji, then the catalog. With
/// a query: one flat list of what matches, names that start with it first.
pub fn rows(query: &str, recent: &[String], custom: &[(String, MediaFile)]) -> Vec<Row> {
    let query = query.trim().trim_matches(':').to_lowercase();
    let custom_cell = |(name, file): &(String, MediaFile)| Cell::Custom { name: name.clone(), file: file.clone() };
    let mut rows = Vec::new();
    if query.is_empty() {
        rows.push(Row::Label("Recent"));
        let named = |emoji: &String| custom.iter().find(|(name, _)| name == emoji).map(custom_cell);
        // A custom emoji used before is drawn only where its image is known: in this conversation.
        let cells = recent.iter().filter_map(|emoji| match emoji.starts_with(':') {
            true => named(emoji),
            false => Some(Cell::Recent(emoji.clone())),
        });
        chunked(&mut rows, cells.collect());
        if !custom.is_empty() {
            rows.push(Row::Label("This conversation"));
            chunked(&mut rows, custom.iter().map(custom_cell).collect());
        }
        rows.push(Row::Label("Emoji"));
        chunked(&mut rows, CATALOG.iter().map(|(glyph, _)| Cell::Glyph(glyph)).collect());
        return rows;
    }
    let mut first = Vec::new();
    let mut rest = Vec::new();
    for pair in custom {
        let name = pair.0.trim_matches(':').to_lowercase();
        match name.find(&query) {
            Some(0) => first.push(custom_cell(pair)),
            Some(_) => rest.push(custom_cell(pair)),
            None => {}
        }
    }
    for (glyph, words) in CATALOG {
        if words.split(' ').any(|word| word.starts_with(&query)) {
            first.push(Cell::Glyph(glyph));
        } else if words.contains(&query) {
            rest.push(Cell::Glyph(glyph));
        }
    }
    first.extend(rest);
    chunked(&mut rows, first);
    rows
}

pub enum PickerEvent {
    Picked(String),
    Closed,
}

impl EventEmitter<PickerEvent> for Picker {}

pub struct Picker {
    input: Entity<TextInput>,
    custom: Vec<(String, MediaFile)>,
    rows: Vec<Row>,
    images: Images,
    _listening: [Subscription; 3],
}

impl Picker {
    /// `custom` is the conversation's own emoji, by `:name:`.
    pub fn new(custom: Vec<(String, MediaFile)>, window: &mut Window, cx: &mut Context<Self>) -> Picker {
        let input = cx.new(|cx| TextInput::new("Search emoji", cx));
        let changed = cx.subscribe(&input, |this: &mut Picker, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => this.requery(cx),
            // Return takes the first match.
            InputEvent::Submit => {
                let first = this.rows.iter().find_map(|row| match row {
                    Row::Cells(cells) => cells.first().map(Cell::emoji),
                    Row::Label(_) => None,
                });
                if let Some(emoji) = first {
                    cx.emit(PickerEvent::Picked(emoji));
                }
            }
        });
        let keys = cx.subscribe(&input, |_, _, key: &InputKey, cx| {
            if *key == InputKey::Escape {
                cx.emit(PickerEvent::Closed);
            }
        });
        // The custom emoji decoded for it go with it.
        let released = cx.on_release_in(window, |this: &mut Picker, window, cx| this.images.clear(window, cx));
        window.focus(&input.focus_handle(cx), cx);
        let rows = rows("", &recents(RECENT), &custom);
        Picker { input, custom, rows, images: Images::default(), _listening: [changed, keys, released] }
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).text().to_owned();
        self.rows = rows(&query, &recents(RECENT), &self.custom);
        cx.notify();
    }

    fn decoded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        if self.images.store(loaded) {
            cx.notify();
        }
        self.images.pump(cx, Self::decoded);
    }

    fn draw_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let edge = media::square_size(24., window.scale_factor());
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let row = match self.rows.get(ix).cloned() {
                Some(Row::Label(words)) => div()
                    .h(px(CELL))
                    .px(px(4.))
                    .pt(px(CELL - LABEL))
                    .flex()
                    .items_center()
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(words.to_uppercase())
                    .into_any_element(),
                Some(Row::Cells(cells)) => {
                    let mut line = div().h(px(CELL)).flex();
                    for (column, cell) in cells.into_iter().enumerate() {
                        let emoji = cell.emoji();
                        let shown = match &cell {
                            Cell::Glyph(glyph) => div().text_size(px(23.)).line_height(px(28.)).child(*glyph).into_any_element(),
                            Cell::Recent(glyph) => {
                                div().text_size(px(23.)).line_height(px(28.)).child(SharedString::from(glyph.clone())).into_any_element()
                            }
                            Cell::Custom { name, file } => {
                                let picture = file.path.as_deref().map(|path| self.images.get(path, edge, None, cx, Self::decoded));
                                match picture {
                                    Some(Picture::Ready(image)) => img(ImageSource::Render(image)).size(px(24.)).into_any_element(),
                                    _ => div()
                                        .w(px(CELL - 4.))
                                        .truncate()
                                        .type_micro()
                                        .text_color(theme::text_soft())
                                        .child(name.trim_matches(':').to_owned())
                                        .into_any_element(),
                                }
                            }
                        };
                        line = line.child(
                            div()
                                .id(("cell", ix * COLUMNS + column))
                                .size(px(CELL))
                                .flex_none()
                                .rounded(px(theme::R_CARD))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::selected()))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(PickerEvent::Picked(emoji.clone()))))
                                .child(shown),
                        );
                    }
                    line.into_any_element()
                }
                None => div().h(px(CELL)).into_any_element(),
            };
            out.push(row);
        }
        out
    }
}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.images.begin_frame();
        let focused = true;
        let grid = uniform_list("emoji", self.rows.len(), cx.processor(Self::draw_rows)).h(px(CELL * ROWS)).w_full();
        let nothing = self.rows.is_empty().then(|| {
            div().h(px(CELL * ROWS)).flex().items_center().justify_center().type_small().text_color(theme::text_faint()).child("No emoji matches")
        });
        div()
            .w(px(WIDTH))
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(field(focused).h(px(32.)).px(px(10.)).flex().items_center().child(self.input.clone()))
            .children(nothing.is_none().then_some(grid))
            .children(nothing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn glyphs(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .flat_map(|row| match row {
                Row::Cells(cells) => cells.iter().map(Cell::emoji).collect(),
                Row::Label(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn recents_come_first_newest_first_and_the_defaults_fill_the_rest() {
        let mut used = Vec::new();
        assert_eq!(filled(&used, 3), ["👍", "❤️", "😂"]);
        bump(&mut used, "🚀");
        bump(&mut used, "❤️");
        bump(&mut used, "🚀");
        assert_eq!(used, ["🚀", "❤️"]);
        assert_eq!(filled(&used, 3), ["🚀", "❤️", "👍"]);
        for n in 0..40 {
            bump(&mut used, &n.to_string());
        }
        assert_eq!(used.len(), RECENT);
    }

    #[test]
    fn a_search_is_one_flat_list_with_names_that_start_with_it_first() {
        let parrot = (":mossparrot:".to_owned(), MediaFile::new("e1", MediaKind::Emoji));
        let custom = [parrot];
        let found = glyphs(&rows("par", &[], &custom));
        // "party" and "partying" start a word; ":mossparrot:" and "sparkles" only contain it.
        assert_eq!(found, ["🎉", "🥳", ":mossparrot:", "✨"]);
        assert_eq!(glyphs(&rows(":moss", &[], &custom)), [":mossparrot:"]);
        assert!(rows("zzzz", &[], &custom).is_empty());

        let all = rows("", &["🚀".to_owned(), ":mossparrot:".to_owned(), ":elsewhere:".to_owned()], &custom);
        assert_eq!(all[0], Row::Label("Recent"));
        // A custom emoji from another conversation has no image here and is left out.
        assert_eq!(glyphs(&all[1..2]), ["🚀", ":mossparrot:"]);
        assert!(all.iter().all(|row| !matches!(row, Row::Cells(cells) if cells.len() > COLUMNS)));
        assert_eq!(glyphs(&all).len(), 2 + 1 + CATALOG.len());
    }
}
