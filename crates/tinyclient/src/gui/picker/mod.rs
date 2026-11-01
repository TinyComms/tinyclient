//! The one picker: Emoji, GIFs and Stickers, each tab only where the account has it. The
//! composer opens it from its smile and GIF buttons; a reaction opens the Emoji tab alone. It is
//! built when it opens and dropped when it closes, with every picture it decoded.
//!
//! Nothing is asked before a tab opens: custom emoji, GIFs and stickers are each one `Catalog`
//! request when their tab is first shown, and a GIF search waits 300 ms and two letters, a new
//! one dropping the old. The grids are `uniform_list`s, so only the rows on screen are built and
//! only their pictures are decoded or fetched. Every cell is a still; the one GIF under the
//! pointer plays after it rests there, never under Reduce Motion, and only when its frames
//! decoded fit 8 MB.
//!
//! Keys: ↓ from the search goes into the grid; arrows move by cell (↑↓ to the nearest cell of
//! the next row, keeping the column); Page Up/Down and ⌘↑/⌘↓ go by section; Return chooses and
//! closes, ⇧Return chooses and keeps it open; F stars the chosen GIF or sticker (`kept`).

pub mod emoji;
mod kept;
#[allow(dead_code)]
mod table;

// One decoder and one picture budget for the whole window, shared with the conversation.
pub(in crate::gui) use crate::gui::conversation::images;

use palette::IntoColor;
use crate::frames;
use crate::gui::anim::{self, Play};
use crate::gui::components::{field, glyph, segment, segmented};
use crate::gui::input::{self, InputEvent, InputGrid, InputKey, TextInput};
use crate::gui::people::ask_later;
use crate::gui::theme::{self, Type};
use emoji::{Cell, Custom, Row, Sections, COLUMNS, TONES};
use gpui::{
    canvas, div, list, prelude::*, px, uniform_list, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    ListAlignment, ListOffset, ListState, ObjectFit, ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window,
};
use kept::List as Kept;
use images::{Images, Loaded, Picture};
use protocol::{AccountKey, CatalogItem, CatalogKind, EmojiScope, ErrorCode, Key, MediaFile, Network, Pick, ReactionRules, Request, Response};
use std::collections::HashSet;

use std::time::Duration;

pub const WIDTH: f32 = 352.;
pub const HEIGHT: f32 = 400.;
pub const MIN_HEIGHT: f32 = 240.;
const TABS: f32 = 40.;
const SEARCH: f32 = 36.;
const STRIP: f32 = 32.;
const FOOTER: f32 = 36.;
const CELL: f32 = 36.;
const GLYPH: f32 = 22.;
const PAD: f32 = 10.;
const GIF_ROW: f32 = 96.;
const GIF_GAP: f32 = 4.;
const STICKER: f32 = 80.;
const STICKER_COLUMNS: usize = 4;
const STICKER_GAP: f32 = 3.;
/// A section label in the GIF and sticker grids.
const LABEL: f32 = 24.;
/// A GIF search waits for the keys to rest this long.
const QUIET: Duration = Duration::from_millis(300);

/// One tab of the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Emoji,
    Gifs,
    Stickers,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Emoji => "Emoji",
            Tab::Gifs => "GIFs",
            Tab::Stickers => "Stickers",
        }
    }

    fn kind(self) -> CatalogKind {
        match self {
            Tab::Emoji => CatalogKind::Emoji,
            Tab::Gifs => CatalogKind::Gifs,
            Tab::Stickers => CatalogKind::Stickers,
        }
    }
}

pub enum PickerEvent {
    /// A Unicode emoji, or under reactions a custom one's `:name:`.
    Picked(String),
    /// A custom emoji, for the composer to put in the field as a token.
    Custom { name: String, id: String, animated: bool },
    /// A GIF or a sticker: sent at once.
    Send(Pick),
    /// One of the above chosen with Shift-Return: the picker stays open.
    Kept(Box<PickerEvent>),
    Closed,
}

impl EventEmitter<PickerEvent> for Picker {}

/// What the picker is opened for.
pub struct Setup {
    pub account: Option<AccountKey>,
    pub network: Option<Network>,
    /// The Space custom emoji are narrowed to, when the account may send only its own.
    pub space: Option<Key>,
    pub tabs: Vec<Tab>,
    pub open: Tab,
    /// Ask the account's custom emoji.
    pub custom: bool,
    /// Custom emoji already known with their files: a reaction's conversation has shown them.
    pub known: Vec<(String, MediaFile)>,
    pub reacting: bool,
    /// For a reaction: what the conversation takes, when chatd said. Only that is offered.
    pub rules: Option<ReactionRules>,
}

/// A GIF or sticker grid row: items by index, with their drawn widths.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    label: Option<String>,
    items: Vec<(usize, f32)>,
}

struct Catalog {
    items: Vec<CatalogItem>,
    cursor: Option<String>,
    /// The query the items answer, and the one being asked now.
    answered: String,
    asking: Option<String>,
    failed: Option<String>,
    task: Option<Task<()>>,
}

impl Catalog {
    fn new() -> Catalog {
        Catalog { items: Vec::new(), cursor: None, answered: String::new(), asking: None, failed: None, task: None }
    }
}

pub struct Picker {
    setup: Setup,
    tab: Tab,
    input: Entity<TextInput>,
    height: f32,
    /// [`WIDTH`] in a popover; a phone's sheet makes it the window's width.
    width: f32,
    customs: Vec<Custom>,
    recent: Vec<String>,
    rows: Vec<Row>,
    sections: Sections,
    section: Option<usize>,
    tone: usize,
    choosing_tone: bool,
    hovered: Option<Cell>,
    /// The keyboard's cell, by (row, column).
    selected: Option<(usize, usize)>,
    custom_asked: bool,
    gifs: Catalog,
    stickers: Catalog,
    lines: Vec<Line>,
    /// The sticker pack the strip picked.
    pack: Option<String>,
    hovered_item: Option<usize>,
    /// Starred GIFs and stickers, and the stickers sent last, by id, newest first.
    favorites: Vec<String>,
    recent_stickers: Vec<String>,
    /// The GIF and sticker grid: rows of one height under labels of another.
    items_list: ListState,
    images: Images,
    /// Files asked for once, so a still that has not landed is not asked again each frame.
    fetched: HashSet<String>,
    scroll: UniformListScrollHandle,
    _listening: Vec<Subscription>,
}

impl Picker {
    /// The reaction picker: Emoji only, with the conversation's own custom emoji, and with
    /// rules only what they allow, the account's custom emoji among it where they may be used.
    pub fn new(
        known: Vec<(String, MediaFile)>,
        rules: Option<ReactionRules>,
        account: Option<AccountKey>,
        space: Option<Key>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Picker {
        let setup = Setup {
            network: account.as_ref().map(|account| account.network),
            account,
            space: None,
            tabs: vec![Tab::Emoji],
            open: Tab::Emoji,
            custom: false,
            known,
            reacting: true,
            rules: None,
        };
        let mut picker = Picker::open(setup, window, cx);
        picker.set_rules(rules, space, cx);
        picker
    }

    /// What the conversation takes arrived, or changed. `space` is where its custom emoji are
    /// from, for a scope of one Space.
    pub fn set_rules(&mut self, rules: Option<ReactionRules>, space: Option<Key>, cx: &mut Context<Self>) {
        let Some(rules) = rules else {
            return;
        };
        // Emoji seen on messages may be another Space's; with a narrower scope than everything
        // the account's own catalog says which may be used.
        if rules.custom != EmojiScope::All {
            self.customs.retain(|custom| !custom.id.is_empty());
        }
        if !emoji::tones_allowed(&rules) {
            self.tone = 0;
            self.choosing_tone = false;
        }
        self.setup.custom = rules.custom != EmojiScope::None && self.setup.account.is_some();
        self.setup.space = if rules.custom == EmojiScope::Space { space } else { None };
        self.setup.rules = Some(rules);
        if self.setup.custom && !self.custom_asked {
            self.custom_asked = true;
            self.ask(Tab::Emoji, String::new(), None, cx);
        }
        self.requery(cx);
    }

    fn tones(&self) -> bool {
        self.setup.rules.as_ref().is_none_or(emoji::tones_allowed)
    }

    pub fn open(setup: Setup, window: &mut Window, cx: &mut Context<Self>) -> Picker {
        let input = cx.new(|cx| TextInput::new("", cx));
        let changed = cx.subscribe(&input, |this: &mut Picker, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => this.requery(cx),
            InputEvent::Submit => this.choose(false, cx),
        });
        let keys = cx.subscribe(&input, |this: &mut Picker, _, key: &InputKey, cx| match key {
            InputKey::Escape => cx.emit(PickerEvent::Closed),
            InputKey::Down => this.step(1, cx),
            InputKey::Up => this.step(-1, cx),
        });
        let grid = cx.subscribe(&input, |this: &mut Picker, _, key: &InputGrid, cx| match key {
            InputGrid::Left => this.across(-1, cx),
            InputGrid::Right => this.across(1, cx),
            InputGrid::SectionUp => this.section_step(false, cx),
            InputGrid::SectionDown => this.section_step(true, cx),
            InputGrid::Keep => this.choose(true, cx),
        });
        input.update(cx, |input, _| input.set_grid(true));
        // What it decoded goes with it.
        let released = cx.on_release_in(window, |this: &mut Picker, window, cx| {
            this.images.clear(window, cx);
        });
        window.focus(&input.focus_handle(cx), cx);
        let customs = setup
            .known
            .iter()
            .map(|(name, file)| Custom { name: name.clone(), id: String::new(), file: Some(file.clone()), pack: "This conversation".into(), animated: false })
            .collect();
        let tab = if setup.tabs.contains(&setup.open) { setup.open } else { Tab::Emoji };
        let (favorites, recent_stickers) = match &setup.account {
            Some(account) => (kept::list(account, Kept::Favorites), kept::list(account, Kept::RecentStickers)),
            None => (Vec::new(), Vec::new()),
        };
        let mut picker = Picker {
            setup,
            tab,
            input,
            height: HEIGHT,
            width: WIDTH,
            customs,
            recent: emoji::recent(),
            rows: Vec::new(),
            sections: Vec::new(),
            section: None,
            tone: emoji::tone(),
            choosing_tone: false,
            hovered: None,
            selected: None,
            custom_asked: false,
            gifs: Catalog::new(),
            stickers: Catalog::new(),
            lines: Vec::new(),
            pack: None,
            hovered_item: None,
            favorites,
            recent_stickers,
            items_list: ListState::new(0, ListAlignment::Top, px(GIF_ROW)),
            // Many cells show at once: each moving one holds little.
            images: Images::with_limits(frames::SMALL),
            fetched: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            _listening: vec![changed, keys, grid, released],
        };
        picker.switch(tab, cx);
        picker
    }

    /// `--composer picker-search` and `picker-tone`: a query typed, or the swatches open.
    #[cfg(feature = "gui-shot")]
    pub fn stage(&mut self, search: bool, tone: bool, cx: &mut Context<Self>) {
        if search {
            self.input.update(cx, |input, cx| input.insert("heart", cx));
        }
        self.choosing_tone = tone;
        cx.notify();
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The height the window leaves it, from its placement.
    pub fn set_height(&mut self, height: f32, cx: &mut Context<Self>) {
        let height = height.clamp(MIN_HEIGHT.min(HEIGHT), HEIGHT);
        if (self.height - height).abs() > 0.5 {
            self.height = height;
            cx.notify();
        }
    }

    /// The width it is shown at: a phone's sheet spans the window, and the grid sits centred in
    /// it. Never narrower than [`WIDTH`]'s grid.
    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = width.max(WIDTH);
        if (self.width - width).abs() > 0.5 {
            self.width = width;
            self.requery(cx);
            cx.notify();
        }
    }

    /// The bands' side padding: the popover's [`PAD`], or, in a phone's wider sheet, the grid's
    /// own edge, so the tabs, the search field, the section strip and the footer line up with
    /// the emoji under them.
    fn side(&self) -> f32 {
        if self.width > WIDTH + 0.5 {
            self.grid_pad()
        } else {
            PAD
        }
    }

    /// The emoji grid's side padding: the popover's 6, or what centres its columns when wider.
    fn grid_pad(&self) -> f32 {
        if self.width > WIDTH + 0.5 {
            ((self.width - COLUMNS as f32 * CELL) / 2.).floor().max(PAD - 4.)
        } else {
            PAD - 4.
        }
    }

    /// Shows another tab; its catalog is asked the first time it opens.
    pub fn switch(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if !self.setup.tabs.contains(&tab) {
            return;
        }
        self.tab = tab;
        self.select(None, cx);
        self.hovered = None;
        self.hovered_item = None;
        self.lines.clear();
        self.items_list.reset(0);
        let placeholder = match (tab, self.setup.network) {
            (Tab::Emoji, _) => "Search emoji".to_owned(),
            (Tab::Gifs, Some(network)) => format!("Search GIFs on {}", network.label()),
            (Tab::Gifs, None) => "Search GIFs".to_owned(),
            (Tab::Stickers, _) => "Search stickers".to_owned(),
        };
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder);
            input.set_text("", cx);
        });
        match tab {
            Tab::Emoji if self.setup.custom && !self.custom_asked => {
                self.custom_asked = true;
                self.ask(Tab::Emoji, String::new(), None, cx);
            }
            Tab::Gifs if self.gifs.items.is_empty() && self.gifs.asking.is_none() => self.ask(Tab::Gifs, String::new(), None, cx),
            Tab::Stickers if self.stickers.items.is_empty() && self.stickers.asking.is_none() => self.ask(Tab::Stickers, String::new(), None, cx),
            _ => {}
        }
        self.requery(cx);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn cycle(&mut self, cx: &mut Context<Self>) {
        let at = self.setup.tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let next = self.setup.tabs[(at + 1) % self.setup.tabs.len()];
        self.switch(next, cx);
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_owned()
    }

    /// Rebuilds the grid for what is typed. GIFs are asked of the network once the keys rest.
    fn requery(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.select(None, cx);
        match self.tab {
            Tab::Emoji => {
                (self.rows, self.sections) = match (&self.setup.rules, query.is_empty()) {
                    (None, true) => emoji::browse(&self.recent, &self.customs),
                    (None, false) => (emoji::search_rows(&query, &self.recent, &self.customs), Vec::new()),
                    (Some(rules), true) => emoji::browse_for(rules, &self.recent, &self.customs, self.tone),
                    (Some(rules), false) => (emoji::search_for(rules, &query, &self.recent, &self.customs, self.tone), Vec::new()),
                };
            }
            Tab::Gifs => {
                let wanted = if query.chars().count() >= 2 { query.clone() } else { String::new() };
                if wanted != self.gifs.answered && self.gifs.asking.as_ref() != Some(&wanted) {
                    let this = cx.weak_entity();
                    self.gifs.asking = Some(wanted.clone());
                    // Replaces the one waiting: a new query cancels the old.
                    self.gifs.task = Some(cx.spawn(async move |_, cx| {
                        cx.background_executor().timer(QUIET).await;
                        let _ = this.update(cx, |this, cx| this.ask(Tab::Gifs, wanted, None, cx));
                    }));
                }
                let sectioned = self.gifs.answered.is_empty();
                self.set_lines(gif_lines(&self.gifs.items, self.width - 2. * PAD, sectioned, &self.favorites));
            }
            Tab::Stickers => {
                let lines = sticker_lines(&self.stickers.items, &query, self.pack.as_deref(), &self.favorites, &self.recent_stickers);
                self.set_lines(lines);
            }
        }
        cx.notify();
    }

    /// The grid's rows: a page added keeps the scroll, a new answer starts at the top.
    fn set_lines(&mut self, lines: Vec<Line>) {
        if lines == self.lines {
            return;
        }
        let same = self.lines.iter().zip(&lines).take_while(|(was, now)| was == now).count();
        let old = self.lines.len();
        self.lines = lines;
        match same {
            0 => self.items_list.reset(self.lines.len()),
            same => self.items_list.splice(same..old, self.lines.len() - same),
        }
    }

    /// One `Catalog` request. A GIF answer for a query no longer typed is dropped.
    fn ask(&mut self, tab: Tab, query: String, cursor: Option<String>, cx: &mut Context<Self>) {
        let Some(account) = self.setup.account.clone() else {
            return;
        };
        let space = if tab == Tab::Emoji { self.setup.space.clone() } else { None };
        let request = Request::Catalog { account, kind: tab.kind(), space, query: query.clone(), cursor: cursor.clone() };
        if tab != Tab::Emoji {
            self.catalog(tab).asking = Some(query.clone());
        }
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            let _ = this.update(cx, |this, cx| this.answered(tab, query, cursor.is_some(), answer, cx));
        });
    }

    fn catalog(&mut self, tab: Tab) -> &mut Catalog {
        match tab {
            Tab::Stickers => &mut self.stickers,
            _ => &mut self.gifs,
        }
    }

    fn answered(&mut self, tab: Tab, query: String, more: bool, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        if tab == Tab::Emoji {
            if let Ok(Response::Catalog { items, .. }) = answer {
                let customs = items.into_iter().map(|item| Custom {
                    name: if item.name.starts_with(':') { item.name } else { format!(":{}:", item.name) },
                    id: item.id,
                    file: item.file,
                    pack: item.pack,
                    animated: item.animated,
                });
                self.customs = customs.collect();
                // Every picture is asked for as the tab opens, so it shows as soon as it lands.
                let want = self.customs.iter().filter_map(|custom| custom.file.as_ref()).filter(|file| file.path.is_none()).map(|file| file.id.clone()).collect();
                self.fetch(want, 48, cx);
                self.requery(cx);
            }
            return;
        }
        let catalog = self.catalog(tab);
        if catalog.asking.as_ref() != Some(&query) {
            return;
        }
        catalog.asking = None;
        let mut want = None;
        match answer {
            Ok(Response::Catalog { items, cursor }) => {
                if !more {
                    catalog.items.clear();
                }
                want = Some(items.iter().filter_map(|item| item.file.as_ref()).filter(|file| file.path.is_none()).map(|file| file.id.clone()).collect());
                catalog.items.extend(items);
                catalog.cursor = cursor;
                catalog.answered = query;
                catalog.failed = None;
            }
            Ok(Response::Error { code: ErrorCode::Offline, .. }) => catalog.failed = Some("offline".into()),
            Ok(Response::Error { detail, .. }) => catalog.failed = Some(detail),
            Ok(_) => catalog.failed = Some(String::new()),
            Err(_) => catalog.failed = Some("tinyclient's background process is not answering.".into()),
        }
        if let Some(want) = want {
            let edge = if tab == Tab::Gifs { GIF_ROW * 2. } else { STICKER * 2. };
            self.fetch(want, edge as u32, cx);
        }
        self.requery(cx);
    }

    /// A file the picker asked for landed.
    pub fn media_landed(&mut self, file: &MediaFile, cx: &mut Context<Self>) {
        let mut hit = false;
        let mut set = |held: &mut Option<MediaFile>| {
            if let Some(held) = held.as_mut().filter(|held| held.id == file.id) {
                held.path = file.path.clone();
                hit = true;
            }
        };
        for item in self.gifs.items.iter_mut().chain(self.stickers.items.iter_mut()) {
            set(&mut item.file);
        }
        for custom in &mut self.customs {
            set(&mut custom.file);
        }
        if hit {
            cx.notify();
        }
    }

    fn fetch(&mut self, ids: Vec<String>, edge: u32, cx: &mut Context<Self>) {
        let Some(account) = self.setup.account.clone() else {
            return;
        };
        let ids: Vec<String> = ids.into_iter().filter(|id| self.fetched.insert(id.clone())).collect();
        if ids.is_empty() {
            return;
        }
        let request = Request::Fetch { account, ids, size: Some(edge), retry: false };
        let this = cx.weak_entity();
        ask_later(cx, request, move |answer, cx| {
            if let Ok(Response::Media { files }) = answer {
                let _ = this.update(cx, |this, cx| {
                    for file in files.iter().filter(|file| file.path.is_some()) {
                        this.media_landed(file, cx);
                    }
                });
            }
        });
    }

    fn decoded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        if self.images.store(loaded) {
            cx.notify();
        }
        self.images.pump(cx, Self::decoded);
    }

    // Choosing.

    /// Sends what a choice made; Shift-Return keeps the picker open, except for a reaction.
    fn chosen(&mut self, event: PickerEvent, keep: bool, cx: &mut Context<Self>) {
        match keep && !self.setup.reacting {
            true => cx.emit(PickerEvent::Kept(Box::new(event))),
            false => cx.emit(event),
        }
    }

    fn emit_cell(&mut self, cell: &Cell, keep: bool, cx: &mut Context<Self>) {
        let text = emoji::text_of(cell, self.tone, &self.customs);
        if text.is_empty() {
            return;
        }
        let custom = match cell {
            Cell::Custom(ix) => self.customs.get(*ix as usize).cloned(),
            _ => None,
        };
        if !self.setup.reacting {
            emoji::used(&text);
        }
        let event = match custom {
            Some(custom) if !self.setup.reacting && !custom.id.is_empty() => PickerEvent::Custom { name: custom.name, id: custom.id, animated: custom.animated },
            // One from the catalog is not on the message yet: its id says which it is.
            Some(custom) if self.setup.reacting => PickerEvent::Picked(emoji::reaction_text(&custom)),
            _ => PickerEvent::Picked(text),
        };
        self.chosen(event, keep, cx);
    }

    fn emit_item(&mut self, ix: usize, keep: bool, cx: &mut Context<Self>) {
        let (tab, items) = match self.tab {
            Tab::Gifs => (Tab::Gifs, &self.gifs.items),
            Tab::Stickers => (Tab::Stickers, &self.stickers.items),
            Tab::Emoji => return,
        };
        let Some(id) = items.get(ix).map(|item| item.id.clone()) else {
            return;
        };
        if let (Tab::Stickers, Some(account)) = (tab, &self.setup.account) {
            kept::sent_sticker(account, &id);
            self.recent_stickers = kept::list(account, Kept::RecentStickers);
        }
        self.chosen(PickerEvent::Send(Pick { kind: tab.kind(), id }), keep, cx);
    }

    /// Return: the keyboard's cell, else the first. Shift-Return keeps the picker open.
    fn choose(&mut self, keep: bool, cx: &mut Context<Self>) {
        let (row, column) = self.selected.unwrap_or((0, 0));
        match self.tab {
            Tab::Emoji => {
                let cell = self.rows.iter().skip(row).find_map(|found| match found {
                    Row::Cells(cells) => cells.get(column).or(cells.first()).cloned(),
                    Row::Label(_) => None,
                });
                if let Some(cell) = cell {
                    self.emit_cell(&cell, keep, cx);
                }
            }
            Tab::Gifs | Tab::Stickers => {
                if let Some(ix) = self.item_at(row, column) {
                    self.emit_item(ix, keep, cx);
                }
            }
        }
    }

    /// The GIF or sticker at a place in the grid, or the first of the rows after it.
    fn item_at(&self, row: usize, column: usize) -> Option<usize> {
        self.lines.iter().skip(row).find_map(|line| line.items.get(column).or(line.items.first()).map(|(ix, _)| *ix))
    }

    /// The keyboard's cell, and whether the field gives the sideways arrows to the grid.
    fn select(&mut self, selected: Option<(usize, usize)>, cx: &mut Context<Self>) {
        self.selected = selected;
        self.input.update(cx, |input, _| input.set_in_grid(selected.is_some()));
        if let Some((row, _)) = selected {
            match self.tab {
                Tab::Emoji => self.scroll.scroll_to_item(row, ScrollStrategy::Nearest),
                _ => self.items_list.scroll_to_reveal_item(row),
            }
        }
        self.hovered = None;
        cx.notify();
    }

    /// The rows that hold cells, by index.
    fn grid_rows(&self) -> Vec<usize> {
        match self.tab {
            Tab::Emoji => self.rows.iter().enumerate().filter(|(_, row)| matches!(row, Row::Cells(_))).map(|(ix, _)| ix).collect(),
            _ => self.lines.iter().enumerate().filter(|(_, line)| !line.items.is_empty()).map(|(ix, _)| ix).collect(),
        }
    }

    /// Each cell's width in a row, and the gap between them.
    fn widths(&self, row: usize) -> (Vec<f32>, f32) {
        match self.tab {
            Tab::Emoji => match self.rows.get(row) {
                Some(Row::Cells(cells)) => (vec![CELL; cells.len()], 0.),
                _ => (Vec::new(), 0.),
            },
            tab => {
                let widths = self.lines.get(row).map(|line| line.items.iter().map(|(_, width)| *width).collect()).unwrap_or_default();
                (widths, if tab == Tab::Gifs { GIF_GAP } else { STICKER_GAP })
            }
        }
    }

    /// Down from the search field moves into the grid, a row at a time, to the cell nearest the
    /// one left; Up off the first row returns to the field.
    fn step(&mut self, by: i32, cx: &mut Context<Self>) {
        let rows = self.grid_rows();
        let at = self.selected.and_then(|(row, _)| rows.iter().position(|ix| *ix == row));
        let next = match (at, by > 0) {
            (None, true) => rows.first().copied(),
            (None, false) => None,
            (Some(at), true) => rows.get(at + 1).or(rows.get(at)).copied(),
            (Some(0), false) => None,
            (Some(at), false) => rows.get(at - 1).copied(),
        };
        let column = match (self.selected, next) {
            (Some((row, column)), Some(to)) => {
                let ((from, gap), (to, _)) = (self.widths(row), self.widths(to));
                nearest(&from, column, &to, gap)
            }
            _ => 0,
        };
        self.select(next.map(|row| (row, column)), cx);
    }

    /// Left and Right: the cell beside, on to the next row's first or the last row's last.
    fn across(&mut self, by: i32, cx: &mut Context<Self>) {
        let Some((row, column)) = self.selected else {
            return;
        };
        let rows = self.grid_rows();
        let counts: Vec<usize> = rows.iter().map(|row| self.widths(*row).0.len()).collect();
        if let Some(next) = across(&rows, &counts, (row, column), by > 0) {
            self.select(Some(next), cx);
        }
    }

    /// Where each section's label row is.
    fn section_starts(&self) -> Vec<usize> {
        match self.tab {
            Tab::Emoji => self.sections.iter().map(|(_, row)| *row).collect(),
            _ => self.lines.iter().enumerate().filter(|(_, line)| line.label.is_some()).map(|(ix, _)| ix).collect(),
        }
    }

    /// Page Up/Down, ⌘↑/⌘↓: the first cell of the previous or next section.
    fn section_step(&mut self, down: bool, cx: &mut Context<Self>) {
        let (starts, rows) = (self.section_starts(), self.grid_rows());
        let Some((target, first)) = section_target(&starts, &rows, self.selected.map(|(row, _)| row), down) else {
            return;
        };
        self.select(Some((first, 0)), cx);
        // The label comes into view with its first row.
        match self.tab {
            Tab::Emoji => {
                self.scroll.scroll_to_item(target, ScrollStrategy::Top);
                self.section = self.sections.iter().position(|(_, row)| *row == target);
            }
            _ => self.items_list.scroll_to(ListOffset { item_ix: target, offset_in_item: px(0.) }),
        }
        cx.notify();
    }

    /// F, or the star on a cell: a GIF or sticker into Favorites or out of it. The keyboard's
    /// cell follows it to where it now sits.
    fn star(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(account) = self.setup.account.clone() else {
            return;
        };
        let items = if self.tab == Tab::Gifs { &self.gifs.items } else { &self.stickers.items };
        let Some(id) = items.get(ix).map(|item| item.id.clone()) else {
            return;
        };
        kept::star(&account, &id);
        self.favorites = kept::list(&account, Kept::Favorites);
        let had = self.selected.is_some();
        self.requery(cx);
        if had {
            let found = self.lines.iter().enumerate().find_map(|(row, line)| line.items.iter().position(|(at, _)| *at == ix).map(|column| (row, column)));
            self.select(found, cx);
        }
    }

    fn star_selected(&mut self, cx: &mut Context<Self>) {
        if self.tab == Tab::Emoji {
            return;
        }
        if let Some(ix) = self.selected.and_then(|(row, column)| self.lines.get(row)?.items.get(column).map(|(ix, _)| *ix)) {
            self.star(ix, cx);
        }
    }

    fn jump(&mut self, section: usize, cx: &mut Context<Self>) {
        if let Some((_, row)) = self.sections.get(section) {
            self.scroll.scroll_to_item(*row, ScrollStrategy::Top);
            self.section = Some(section);
            cx.notify();
        }
    }

    // A GIF plays as the autoplay setting says (`anim`); the hovered cell shows its star.

    fn hover_item(&mut self, ix: usize, on: bool, cx: &mut Context<Self>) {
        if on {
            self.hovered_item = Some(ix);
            cx.notify();
        } else if self.hovered_item == Some(ix) {
            self.hovered_item = None;
            cx.notify();
        }
    }

    // Drawing.

    fn tabs_band(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.setup.tabs.len() < 2 {
            return None;
        }
        let mut control = segmented().w_full();
        for (ix, tab) in self.setup.tabs.iter().enumerate() {
            let tab = *tab;
            control = control.child(
                segment(("picker-tab", ix), tab.label(), tab == self.tab)
                    .flex_1()
                    .justify_center()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.switch(tab, cx);
                        cx.notify();
                    })),
            );
        }
        Some(div().flex_none().h(px(TABS)).px(px(self.side())).pt(px(6.)).child(control).into_any_element())
    }

    fn search_band(&self) -> AnyElement {
        let field = field(true).w_full().child(glyph("icons/search.svg", 14., theme::text_faint())).child(div().flex_1().min_w_0().child(self.input.clone()));
        div().flex_none().h(px(SEARCH)).px(px(self.side())).pb(px(8.)).flex().items_end().child(field).into_any_element()
    }

    fn strip_band(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let button = |id: (&'static str, usize), selected: bool| {
            div()
                .id(id)
                .flex_none()
                .size(px(28.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(selected, |button| button.bg(theme::selected()))
                .hover(|style| style.bg(theme::hover()))
        };
        // A phone's sheet spreads the sections across the grid's width.
        let wide = self.width > WIDTH + 0.5;
        let strip = div().flex_none().h(px(STRIP)).px(px(self.side())).flex().items_center().gap(px(2.)).when(wide, |strip| strip.justify_between()).overflow_hidden();
        match self.tab {
            Tab::Emoji if self.query(cx).is_empty() => {
                let mut strip = strip;
                for (ix, (label, _)) in self.sections.iter().enumerate() {
                    // CONTRACT: the design's line glyphs for the groups (leaf, coffee, …) are not
                    // among slice A's assets; each group shows its first emoji, dimmed until chosen.
                    let mark = match label.as_str() {
                        "Frequently used" => glyph("icons/clock.svg", 16., theme::text_soft()).into_any_element(),
                        _ => {
                            let group = table::GROUPS.iter().position(|group| group == label);
                            let glyph = match group {
                                Some(group) => [ "😀", "🐻", "🍔", "🚗", "⚽", "💡", "❤️", "🏁"][group.min(7)],
                                None => "⭐",
                            };
                            let lit = self.section == Some(ix);
                            div().text_size(px(15.)).line_height(px(18.)).opacity(if lit { 1. } else { 0.55 }).child(glyph).into_any_element()
                        }
                    };
                    strip = strip.child(
                        button(("picker-section", ix), self.section == Some(ix))
                            .on_click(cx.listener(move |this, _, _, cx| this.jump(ix, cx)))
                            .child(mark),
                    );
                }
                Some(strip.into_any_element())
            }
            Tab::Stickers => {
                // A pack shows its first sticker; until that lands, the sticker's emoji.
                let mut packs: Vec<(String, Option<String>, String)> = Vec::new();
                let mut want = Vec::new();
                for item in &self.stickers.items {
                    if !packs.iter().any(|(name, ..)| *name == item.pack) {
                        let path = item.file.as_ref().and_then(|file| file.path.clone());
                        if let (None, Some(file)) = (&path, &item.file) {
                            want.push(file.id.clone());
                        }
                        packs.push((item.pack.clone(), path, item.name.clone()));
                    }
                }
                self.fetch(want, (STICKER * 2.) as u32, cx);
                let mut strip = strip.child(
                    button(("picker-pack", 0), self.pack.is_none())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pack = None;
                            this.requery(cx);
                        }))
                        .child(glyph("icons/clock.svg", 16., theme::text_soft())),
                );
                for (ix, (name, path, sticker)) in packs.into_iter().enumerate() {
                    let picture = match path {
                        Some(path) => self.thumb(&path, 20., Some(&sticker), cx),
                        None => standin(&sticker, 20.),
                    };
                    let shown = self.pack.as_deref() == Some(name.as_str());
                    strip = strip.child(
                        button(("picker-pack", ix + 1), shown)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pack = Some(name.clone());
                                this.requery(cx);
                            }))
                            .child(picture),
                    );
                }
                Some(strip.into_any_element())
            }
            _ => None,
        }
    }

    fn thumb(&mut self, path: &str, size: f32, glyph: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let edge = (size * 2.).ceil() as u32;
        match self.images.get(path, edge, None, cx, Self::decoded) {
            Picture::Ready(image) => div().size(px(size)).child(anim::picture(image, ObjectFit::Fill, 4., Play::Inline)).into_any_element(),
            _ => standin(glyph.unwrap_or(""), size),
        }
    }

    fn footer_band(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let footer = div()
            .flex_none()
            .h(px(FOOTER))
            .px(px(self.side()))
            .border_t_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .type_small()
            .text_color(theme::text_soft());
        match self.tab {
            Tab::Emoji => {
                if self.choosing_tone {
                    let mut swatches = footer.child(div().flex_1().text_color(theme::text_faint()).child("Skin tone"));
                    for tone in 0..TONES.len() {
                        let hand = emoji::toned(&table::EMOJI[emoji::find("✋").unwrap_or(0)], tone);
                        swatches = swatches.child(
                            div()
                                .id(("tone", tone))
                                .size(px(24.))
                                .rounded(px(theme::R_CONTROL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .when(tone == self.tone, |swatch| swatch.bg(theme::selected()))
                                .hover(|style| style.bg(theme::hover()))
                                .text_size(px(16.))
                                .child(hand)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.tone = tone;
                                    this.choosing_tone = false;
                                    emoji::set_tone(tone);
                                    cx.notify();
                                })),
                        );
                    }
                    return swatches.into_any_element();
                }
                let shown = self.hovered.clone().or_else(|| self.selected_cell());
                let preview = shown.map(|cell| {
                    let text = emoji::text_of(&cell, self.tone, &self.customs);
                    let name = emoji::name_of(&cell, &self.customs);
                    let pack = match &cell {
                        Cell::Custom(ix) => self.customs.get(*ix as usize).map(|custom| custom.pack.clone()).unwrap_or_default(),
                        _ => String::new(),
                    };
                    let big = match &cell {
                        Cell::Custom(ix) => {
                            let path = self.customs.get(*ix as usize).and_then(|custom| custom.file.as_ref()?.path.clone());
                            match path.map(|path| self.images.get(&path, 48, None, cx, Self::decoded)) {
                                Some(Picture::Ready(image)) => div().size(px(GLYPH)).child(anim::picture(image, ObjectFit::Fill, 0., Play::Inline)).into_any_element(),
                                _ => standin("", GLYPH),
                            }
                        }
                        _ => div().text_size(px(GLYPH)).line_height(px(26.)).child(text).into_any_element(),
                    };
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(big)
                        .child(div().min_w_0().truncate().type_ui().text_color(theme::text()).child(name))
                        .when(!pack.is_empty(), |line| line.child(div().flex_none().text_color(theme::text_soft()).child(pack)))
                });
                let hand = emoji::toned(&table::EMOJI[emoji::find("✋").unwrap_or(0)], self.tone);
                let tone = div()
                    .id("tone-open")
                    .flex_none()
                    .size(px(28.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .text_size(px(18.))
                    .child(hand)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choosing_tone = true;
                        cx.notify();
                    }));
                // A list of its own: say so where nothing is hovered.
                let only = self.setup.rules.as_ref().is_some_and(|rules| rules.set == protocol::ReactionSet::Only);
                let idle = || match only {
                    true => div().flex_1().min_w_0().truncate().text_color(theme::text_faint()).child("Only the reactions allowed here"),
                    false => div().flex_1(),
                };
                let tones = self.tones();
                footer.child(preview.unwrap_or_else(idle)).when(tones, |footer| footer.child(tone)).into_any_element()
            }
            Tab::Gifs => {
                let network = self.setup.network.map_or("the network", |network| network.label());
                let words = match &self.gifs.asking {
                    Some(query) if !query.is_empty() => format!("Searching {network} for “{query}”…"),
                    _ if self.setup.network == Some(Network::Telegram) => "Searches go to @gif".to_owned(),
                    _ => match gif_provider(&self.gifs.items, self.setup.network) {
                        Some(provider) => format!("Searches go to {network} · via {provider}"),
                        None => format!("Searches go to {network}"),
                    },
                };
                footer.child(div().truncate().child(words)).into_any_element()
            }
            Tab::Stickers => {
                let item = self.hovered_item.and_then(|ix| self.stickers.items.get(ix));
                let words = item.map(|item| format!("{} {}", item.name, item.pack)).unwrap_or_default();
                footer.child(div().truncate().child(words)).into_any_element()
            }
        }
    }

    fn selected_cell(&self) -> Option<Cell> {
        let (row, column) = self.selected?;
        match self.rows.get(row)? {
            Row::Cells(cells) => cells.get(column).or(cells.first()).cloned(),
            Row::Label(_) => None,
        }
    }

    fn empty(&self, height: f32, cx: &App) -> Option<AnyElement> {
        let network = self.setup.network.map_or("the network", |network| network.label());
        let query = self.query(cx);
        let (icon, title, line) = match self.tab {
            Tab::Emoji if self.rows.is_empty() => ("icons/search.svg", format!("No emoji match “{query}”"), String::new()),
            Tab::Emoji => return None,
            tab => {
                let catalog = if tab == Tab::Gifs { &self.gifs } else { &self.stickers };
                if !self.lines.is_empty() {
                    return None;
                }
                match (&catalog.failed, &catalog.asking) {
                    (Some(why), _) if why == "offline" => (
                        "icons/wifi-off.svg",
                        format!("{} need a connection", tab.label()),
                        if tab == Tab::Gifs { format!("Favorites come back with {network}.") } else { String::new() },
                    ),
                    (Some(why), _) => ("icons/circle-alert.svg", format!("Couldn't load {}", tab.label()), why.clone()),
                    (None, Some(_)) => return Some(div().h(px(height)).into_any_element()),
                    (None, None) if query.is_empty() => ("icons/image.svg", format!("No {} yet", tab.label()), String::new()),
                    (None, None) => ("icons/search.svg", format!("No {} for “{query}”", tab.label()), String::new()),
                }
            }
        };
        Some(
            div()
                .h(px(height))
                .px(px(24.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(glyph(icon, 20., theme::text_faint()))
                .child(div().mt(px(4.)).type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(title))
                .when(!line.is_empty(), |empty| empty.child(div().type_small().text_color(theme::text_soft()).child(line)))
                .into_any_element(),
        )
    }

    fn emoji_rows(&mut self, range: std::ops::Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let edge = (24. * window.scale_factor()).ceil() as u32;
        let mut want = Vec::new();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let element = match self.rows.get(ix).cloned() {
                Some(Row::Label(words)) => div()
                    .h(px(CELL))
                    .px(px(4.))
                    .pb(px(4.))
                    .flex()
                    .items_end()
                    .type_micro()
                    .text_color(theme::text_soft())
                    .child(words)
                    .into_any_element(),
                Some(Row::Cells(cells)) => {
                    let mut line = div().h(px(CELL)).flex();
                    for (column, cell) in cells.into_iter().enumerate() {
                        let selected = self.selected == Some((ix, column));
                        let shown = match &cell {
                            Cell::Custom(at) => {
                                let custom = self.customs.get(*at as usize);
                                let path = custom.and_then(|custom| custom.file.as_ref()).and_then(|file| file.path.clone());
                                // A neutral tile until the picture lands; never a cut-off name.
                                match (path, custom) {
                                    (Some(path), _) => match self.images.get(&path, edge, None, cx, Self::decoded) {
                                        Picture::Ready(image) => div().size(px(24.)).child(anim::picture(image, ObjectFit::Fill, 0., Play::Inline)).into_any_element(),
                                        _ => standin("", 24.),
                                    },
                                    (None, Some(custom)) => {
                                        if let Some(file) = &custom.file {
                                            want.push(file.id.clone());
                                        }
                                        standin("", 24.)
                                    }
                                    _ => standin("", 24.),
                                }
                            }
                            _ => div()
                                .text_size(px(GLYPH))
                                .line_height(px(28.))
                                .child(SharedString::from(emoji::text_of(&cell, self.tone, &self.customs)))
                                .into_any_element(),
                        };
                        let picked = cell.clone();
                        let hovered = cell.clone();
                        line = line.child(
                            div()
                                .id(("cell", ix * COLUMNS + column))
                                .size(px(CELL))
                                .flex_none()
                                .rounded(px(theme::R_CONTROL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .when(selected, |cell| cell.bg(theme::selected()))
                                .hover(|style| style.bg(theme::selected()))
                                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                                    if *on {
                                        this.hovered = Some(hovered.clone());
                                        cx.notify();
                                    }
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| this.emit_cell(&picked, false, cx)))
                                .child(shown),
                        );
                    }
                    line.into_any_element()
                }
                None => div().h(px(CELL)).into_any_element(),
            };
            out.push(element);
        }
        self.fetch(want, edge, cx);
        out
    }

    /// One row of the GIF or sticker grid: a section label, or its cells.
    fn item_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let gifs = self.tab == Tab::Gifs;
        let scale = window.scale_factor();
        let height = if gifs { GIF_ROW } else { STICKER };
        // The last row is drawn: the next page, if there is one.
        if ix + 1 >= self.lines.len() {
            let tab = self.tab;
            let catalog = self.catalog(tab);
            if let (Some(cursor), None) = (catalog.cursor.clone(), catalog.asking.as_ref()) {
                let query = catalog.answered.clone();
                self.ask(tab, query, Some(cursor), cx);
            }
        }
        let Some(line) = self.lines.get(ix).cloned() else {
            return div().h(px(height)).into_any_element();
        };
        if let Some(label) = line.label {
            return div().h(px(LABEL)).px(px(4.)).pb(px(4.)).flex().items_end().type_micro().text_color(theme::text_soft()).child(label).into_any_element();
        }
        let mut want = Vec::new();
        let gap = if gifs { GIF_GAP } else { STICKER_GAP };
        let mut row = div().h(px(height + gap)).pb(px(gap)).flex().gap(px(gap));
        for (column, (at, width)) in line.items.iter().copied().enumerate() {
            let items = if gifs { &self.gifs.items } else { &self.stickers.items };
            let Some(item) = items.get(at).cloned() else {
                continue;
            };
            let (w, h) = if gifs { (width, GIF_ROW) } else { (STICKER, STICKER) };
            let selected = self.selected == Some((ix, column));
            let hovered = self.hovered_item == Some(at);
            let starred = self.favorites.contains(&item.id);
            let path = item.file.as_ref().and_then(|file| file.path.clone());
            let tile = || div().size_full().rounded(px(theme::R_CONTROL)).bg(theme::raised()).into_any_element();
            let picture = match (&path, &item.file) {
                (Some(path), _) => {
                    let edge = (w.max(h) * scale).ceil() as u32;
                    // Cover: the row's box keeps its height; the little a justified row
                    // stretches it by is cropped.
                    let crop = gifs.then_some(h / w);
                    match self.images.get(path, edge, crop, cx, Self::decoded) {
                        Picture::Ready(image) => div()
                            .w(px(if gifs { w } else { STICKER - 8. }))
                            .h(px(if gifs { h } else { STICKER - 8. }))
                            .child(anim::picture(image, ObjectFit::Fill, theme::R_CONTROL, Play::Inline))
                            .into_any_element(),
                        _ => tile(),
                    }
                }
                (None, Some(file)) => {
                    want.push(file.id.clone());
                    tile()
                }
                (None, None) => tile(),
            };
            let star = (hovered || selected).then(|| {
                div()
                    .id(("star", at))
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
                    .tooltip(crate::gui::components::tip(if starred { "Remove from Favorites  F" } else { "Add to Favorites  F" }, None))
                    .child(glyph("icons/star.svg", 14., if starred { theme::warning().into_color() } else { gpui::white() }))
                    // The star is not the cell: a click here never sends.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.star(at, cx);
                    }))
            });
            row = row.child(
                div()
                    .id(("item", at))
                    .relative()
                    .w(px(w))
                    .h(px(h))
                    .flex_none()
                    .rounded(px(theme::R_CONTROL))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(selected || hovered, |cell| cell.border_2().border_color(if selected { theme::focus() } else { theme::border_strong() }))
                    .when(!gifs, |cell| cell.hover(|style| style.bg(theme::selected())))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| this.hover_item(at, *on, cx)))
                    .on_click(cx.listener(move |this, _, _, cx| this.emit_item(at, false, cx)))
                    .child(picture)
                    .children(star),
            );
        }
        let edge = ((if gifs { GIF_ROW * 2. } else { STICKER }) * scale).ceil() as u32;
        self.fetch(want, edge, cx);
        row.into_any_element()
    }
}

/// A neutral tile for a picture that has not landed: a sticker's emoji when it has one, never
/// a cut-off name.
fn standin(glyph: &str, size: f32) -> AnyElement {
    let tile = div().flex_none().size(px(size)).rounded(px(4.)).bg(theme::raised()).flex().items_center().justify_center();
    match glyph.is_empty() {
        true => tile.into_any_element(),
        false => tile.bg(gpui::transparent_black()).text_size(px(size * 0.8)).line_height(px(size)).child(glyph.to_owned()).into_any_element(),
    }
}

/// Who the GIF search goes through, named in the footer: from the results' own addresses, and
/// on Discord, whose search proxies Tenor, Tenor when they do not say.
fn gif_provider(items: &[CatalogItem], network: Option<Network>) -> Option<&'static str> {
    let named = items.iter().find_map(|item| {
        let host = item.id.split("://").nth(1)?.split('/').next()?.to_ascii_lowercase();
        match () {
            _ if host == "tenor.com" || host.ends_with(".tenor.com") => Some("Tenor"),
            _ if host == "giphy.com" || host.ends_with(".giphy.com") => Some("GIPHY"),
            _ => None,
        }
    });
    named.or((network == Some(Network::Discord)).then_some("Tenor"))
}

/// The cell of a row nearest, across, to the one left in the row before: ↑ and ↓ keep the
/// column, and in justified GIF rows land under the same place.
fn nearest(from: &[f32], column: usize, to: &[f32], gap: f32) -> usize {
    if to.is_empty() {
        return 0;
    }
    let left = |widths: &[f32], at: usize| widths[..at].iter().sum::<f32>() + gap * at as f32;
    let column = column.min(from.len().saturating_sub(1));
    let centre = match from.get(column) {
        Some(width) => left(from, column) + width / 2.,
        None => 0.,
    };
    let distance = |at: usize| (left(to, at) + to[at] / 2. - centre).abs();
    (0..to.len()).min_by(|a, b| distance(*a).total_cmp(&distance(*b))).unwrap_or(0)
}

/// Left and Right from `(row, column)`: the cell beside, else the next row's first or the last
/// row's last. `rows` are the rows with cells and `counts` their lengths.
fn across(rows: &[usize], counts: &[usize], (row, column): (usize, usize), right: bool) -> Option<(usize, usize)> {
    let at = rows.iter().position(|ix| *ix == row)?;
    let count = counts[at];
    match right {
        true if column + 1 < count => Some((row, column + 1)),
        true => rows.get(at + 1).map(|row| (*row, 0)),
        false if column > 0 => Some((row, column.min(count) - 1)),
        false => at.checked_sub(1).map(|at| (rows[at], counts[at].saturating_sub(1))),
    }
}

/// Page Up/Down from the row `current` (none: the search field): the section to show, as its
/// label's row, and the first row of cells in it. Up goes to this section's start, then the one
/// before; Down to the next, or the last row when there is none.
fn section_target(starts: &[usize], rows: &[usize], current: Option<usize>, down: bool) -> Option<(usize, usize)> {
    let target = match (down, current) {
        (true, None) => starts.first().or(rows.first()).copied(),
        (true, Some(row)) => starts.iter().find(|start| **start > row).or(rows.last()).copied(),
        (false, None) => None,
        (false, Some(row)) => {
            let this = starts.iter().rev().find(|start| **start <= row).copied();
            let first_row = this.and_then(|this| rows.iter().find(|ix| **ix >= this)).copied();
            match (this, first_row) {
                // Already on this section's first row: the one before.
                (Some(this), Some(first)) if first == row => starts.iter().rev().find(|start| **start < this).copied().or(Some(this)),
                (Some(this), _) => Some(this),
                (None, _) => rows.first().copied(),
            }
        }
    }?;
    let first = rows.iter().find(|row| **row >= target).copied()?;
    Some((target, first))
}


/// GIFs in justified rows of one height: each keeps its shape, and a full row is stretched
/// to the width, its stills cropped by the little it takes. With no query they sit under their
/// sections: the starred first as "Favorites", then each as the network labels it ("Saved",
/// "Trending"), in the order the network gave them.
fn gif_lines(items: &[CatalogItem], width: f32, sectioned: bool, favorites: &[String]) -> Vec<Line> {
    if !sectioned {
        return justified(items, &(0..items.len()).collect::<Vec<_>>(), width);
    }
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    if items.iter().any(|item| favorites.contains(&item.id)) {
        groups.push(("Favorites", Vec::new()));
    }
    for (ix, item) in items.iter().enumerate() {
        let label = if favorites.contains(&item.id) { "Favorites" } else { item.pack.as_str() };
        match groups.iter_mut().find(|(held, _)| *held == label) {
            Some((_, members)) => members.push(ix),
            None => groups.push((label, vec![ix])),
        }
    }
    let mut lines = Vec::new();
    for (label, members) in groups {
        if !label.is_empty() {
            lines.push(Line { label: Some(label.to_owned()), items: Vec::new() });
        }
        lines.extend(justified(items, &members, width));
    }
    lines
}

fn justified(items: &[CatalogItem], members: &[usize], width: f32) -> Vec<Line> {
    let aspect = |item: &CatalogItem| {
        item.file.as_ref().filter(|file| file.width > 0 && file.height > 0).map_or(1.4, |file| file.width as f32 / file.height as f32).clamp(0.5, 3.)
    };
    let mut lines = Vec::new();
    let mut line: Vec<(usize, f32)> = Vec::new();
    let mut used = 0.;
    for &ix in members {
        let w = aspect(&items[ix]) * GIF_ROW;
        let gaps = GIF_GAP * line.len() as f32;
        if !line.is_empty() && used + gaps + w > width {
            lines.push(justify(std::mem::take(&mut line), used, width, true));
            used = 0.;
        }
        line.push((ix, w));
        used += w;
    }
    if !line.is_empty() {
        lines.push(justify(line, used, width, false));
    }
    lines
}

fn justify(line: Vec<(usize, f32)>, used: f32, width: f32, full: bool) -> Line {
    let room = width - GIF_GAP * (line.len() as f32 - 1.);
    let scale = if full || used > room { room / used } else { 1. };
    Line { label: None, items: line.into_iter().map(|(ix, w)| (ix, (w * scale).floor())).collect() }
}

/// Stickers four to a row: with nothing typed and no pack chosen, "Recent" (the last sent)
/// and "Favorites" first; then each pack under its name, a pack chosen in the strip alone, and a
/// search matching name or emoji.
fn sticker_lines(items: &[CatalogItem], query: &str, pack: Option<&str>, favorites: &[String], recent: &[String]) -> Vec<Line> {
    let query = query.to_lowercase();
    let mut lines = Vec::new();
    let section = |lines: &mut Vec<Line>, label: String, shown: &[usize]| {
        if shown.is_empty() {
            return;
        }
        lines.push(Line { label: Some(label), items: Vec::new() });
        for chunk in shown.chunks(STICKER_COLUMNS) {
            lines.push(Line { label: None, items: chunk.iter().map(|ix| (*ix, STICKER)).collect() });
        }
    };
    if query.is_empty() && pack.is_none() {
        let by_id = |id: &String| items.iter().position(|item| item.id == *id);
        let recent: Vec<usize> = recent.iter().filter_map(by_id).take(kept::RECENT_SHOWN).collect();
        section(&mut lines, "Recent".to_owned(), &recent);
        let starred: Vec<usize> = favorites.iter().filter_map(by_id).collect();
        section(&mut lines, "Favorites".to_owned(), &starred);
    }
    let mut packs: Vec<&str> = Vec::new();
    for item in items {
        if !packs.contains(&item.pack.as_str()) {
            packs.push(&item.pack);
        }
    }
    for name in packs.into_iter().filter(|name| pack.is_none_or(|pack| pack == *name)) {
        let shown: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.pack == name && (query.is_empty() || item.name.to_lowercase().contains(&query) || name.to_lowercase().contains(&query)))
            .map(|(ix, _)| ix)
            .collect();
        let label = format!("{} · {}", if name.is_empty() { "Stickers" } else { name }, shown.len());
        section(&mut lines, label, &shown);
    }
    lines
}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.images.begin_frame();
        let this = cx.entity();
        let tabs = self.tabs_band(cx);
        let strip = self.strip_band(cx);
        let bands = tabs.as_ref().map_or(6., |_| TABS) + SEARCH + strip.as_ref().map_or(0., |_| STRIP) + FOOTER;
        let grid_height = (self.height - bands - 2.).max(CELL);
        let grid = match self.empty(grid_height, cx) {
            Some(empty) => empty,
            None => match self.tab {
                Tab::Emoji => uniform_list("picker-emoji", self.rows.len(), cx.processor(Self::emoji_rows))
                    .track_scroll(&self.scroll)
                    .h(px(grid_height))
                    .w_full()
                    .px(px(self.grid_pad()))
                    .into_any_element(),
                // The list lays its rows edge to edge: the padding is its holder's.
                _ => div()
                    .h(px(grid_height))
                    .w_full()
                    .px(px(PAD))
                    .child(list(self.items_list.clone(), cx.processor(Self::item_row)).size_full())
                    .into_any_element(),
            },
        };
        div()
            .w(px(self.width))
            .h(px(self.height))
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &input::Tab, _, cx| this.cycle(cx)))
            .on_action(cx.listener(|this, _: &input::Star, _, cx| this.star_selected(cx)))
            // F stars only while a GIF or sticker is chosen; otherwise it is typed.
            .when(self.tab != Tab::Emoji && self.selected.is_some() && self.setup.account.is_some(), |picker| picker.key_context("PickerGrid"))
            .children(tabs)
            .when(self.setup.tabs.len() < 2, |picker| picker.pt(px(6.)))
            .child(self.search_band())
            .children(strip)
            .child(grid)
            .child(self.footer_band(cx))
            // After the cells are laid out: frames of cells scrolled away go.
            .child(canvas(|_, _, _| (), move |_, (), window, cx| this.update(cx, |this, cx| this.images.sweep(window, cx))).absolute().size_0())
    }
}

/// A phone's bottom sheet: the scope sheet's surface (raised, 12 pt top corners, the border
/// along its top edge, the modal shadow) with a grabber, and the home indicator's room under
/// whatever the caller puts in it. Full width, at the window's bottom.
pub(in crate::gui) fn sheet_panel() -> gpui::Div {
    use crate::gui::phone::tokens;
    div()
        .w_full()
        .bg(theme::raised())
        .shadow(theme::shadow_modal())
        .rounded_t(px(theme::R_MODAL))
        .border_t_1()
        .border_color(theme::border())
        .pt(px(8.))
        .pb(px(8. + tokens::bottom_padding(tokens::safe_bottom(), tokens::keyboard())))
        .flex()
        .flex_col()
        .occlude()
        // A press on the sheet is not a press on the scrim behind it.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().flex_none().h(px(12.)).flex().justify_center().child(div().w(px(36.)).h(px(4.)).rounded_full().bg(theme::border_strong())))
}

/// What a sheet stands on: the window, dimmed, its content at the bottom. The caller closes the
/// sheet on a press of the returned scrim.
pub(in crate::gui) fn sheet_scrim(window: &Window) -> gpui::Stateful<gpui::Div> {
    let size = window.viewport_size();
    div().id("sheet-scrim").w(size.width).h(size.height).bg(theme::scrim()).occlude().flex().flex_col().justify_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn gif(id: &str, w: u32, h: u32) -> CatalogItem {
        let file = MediaFile { width: w, height: h, ..MediaFile::new(id, MediaKind::Gif) };
        CatalogItem { id: id.into(), name: id.into(), file: Some(file), pack: String::new(), animated: true }
    }

    #[test]
    fn gifs_fill_rows_of_one_height_keeping_their_shape() {
        let items = [gif("a", 200, 100), gif("b", 100, 100), gif("c", 300, 100), gif("d", 100, 200)];
        let lines = gif_lines(&items, 332., false, &[]);
        assert!(lines.len() >= 2);
        for line in &lines[..lines.len() - 1] {
            let used: f32 = line.items.iter().map(|(_, w)| w).sum::<f32>() + GIF_GAP * (line.items.len() as f32 - 1.);
            assert!((used - 332.).abs() <= line.items.len() as f32, "a full row fills the width: {used}");
        }
        let all: Vec<usize> = lines.iter().flat_map(|line| line.items.iter().map(|(ix, _)| *ix)).collect();
        assert_eq!(all, [0, 1, 2, 3], "every GIF once, in order");
        // The last row keeps its natural widths rather than stretching.
        let last = lines.last().unwrap();
        assert!(last.items.iter().all(|(_, w)| *w <= 3. * GIF_ROW));
        assert!(gif_lines(&[], 332., true, &[]).is_empty());
    }

    #[test]
    fn with_no_query_gifs_sit_under_favorites_then_the_network_s_own_sections() {
        let mut items = vec![gif("a", 200, 100), gif("b", 100, 100), gif("c", 300, 100), gif("d", 100, 200)];
        items[0].pack = "Trending".into();
        items[1].pack = "Trending".into();
        items[2].pack = "Saved".into();
        items[3].pack = "Trending".into();
        let lines = gif_lines(&items, 332., true, &["c".to_owned(), "d".to_owned()]);
        let labels: Vec<&str> = lines.iter().filter_map(|line| line.label.as_deref()).collect();
        assert_eq!(labels, ["Favorites", "Trending"], "starred first; a section emptied by stars is gone");
        let favorites: Vec<usize> = lines[1..].iter().take_while(|line| line.label.is_none()).flat_map(|line| line.items.iter().map(|(ix, _)| *ix)).collect();
        assert_eq!(favorites, [2, 3]);
        // A search is one flat grid.
        assert!(gif_lines(&items, 332., false, &["c".to_owned()]).iter().all(|line| line.label.is_none()));
        assert_eq!(gif_provider(&[CatalogItem { id: "https://media.giphy.com/x".into(), ..gif("x", 1, 1) }], Some(Network::Discord)), Some("GIPHY"));
        assert_eq!(gif_provider(&items, Some(Network::Discord)), Some("Tenor"));
        assert_eq!(gif_provider(&items, Some(Network::Telegram)), None);
    }

    #[test]
    fn up_and_down_keep_the_column_and_land_under_the_same_place_in_justified_rows() {
        // Emoji: nine to a row, and a shorter last row.
        let full = [CELL; 9];
        assert_eq!(nearest(&full, 6, &full, 0.), 6);
        assert_eq!(nearest(&full, 6, &[CELL; 3], 0.), 2, "a shorter row: its last cell");
        // GIFs: a wide cell over two narrow ones lands on the one under its middle.
        assert_eq!(nearest(&[200., 128.], 0, &[96., 96., 132.], 4.), 1);
        assert_eq!(nearest(&[200., 128.], 1, &[96., 96., 132.], 4.), 2);
        assert_eq!(nearest(&[96.], 0, &[], 4.), 0);
    }

    #[test]
    fn left_and_right_move_along_the_row_and_on_to_the_next() {
        // Rows 1 and 2 hold cells (0 is a label), 9 and 4 of them.
        let (rows, counts) = ([1, 2], [9, 4]);
        assert_eq!(across(&rows, &counts, (1, 3), true), Some((1, 4)));
        assert_eq!(across(&rows, &counts, (1, 3), false), Some((1, 2)));
        assert_eq!(across(&rows, &counts, (1, 8), true), Some((2, 0)));
        assert_eq!(across(&rows, &counts, (2, 0), false), Some((1, 8)));
        assert_eq!(across(&rows, &counts, (2, 3), true), None, "the last cell stays");
        assert_eq!(across(&rows, &counts, (1, 0), false), None);
    }

    #[test]
    fn page_keys_and_command_arrows_go_by_section() {
        // Labels at 0, 3 and 6; cells in every other row.
        let (starts, rows) = ([0, 3, 6], [1, 2, 4, 5, 7]);
        assert_eq!(section_target(&starts, &rows, None, true), Some((0, 1)), "from the search: the first section");
        assert_eq!(section_target(&starts, &rows, Some(1), true), Some((3, 4)));
        assert_eq!(section_target(&starts, &rows, Some(5), true), Some((6, 7)));
        assert_eq!(section_target(&starts, &rows, Some(7), true), Some((7, 7)), "past the last: its last row");
        assert_eq!(section_target(&starts, &rows, Some(5), false), Some((3, 4)), "inside a section: its start");
        assert_eq!(section_target(&starts, &rows, Some(4), false), Some((0, 1)), "at a start: the one before");
        assert_eq!(section_target(&starts, &rows, None, false), None);
        assert_eq!(section_target(&[], &rows, Some(4), true), Some((7, 7)), "no sections: the end");
    }

    #[test]
    fn stickers_sit_four_to_a_row_under_their_pack_and_filter_by_name() {
        let sticker = |id: &str, name: &str, pack: &str| CatalogItem { id: id.into(), name: name.into(), file: None, pack: pack.into(), animated: false };
        let items: Vec<CatalogItem> = (0..6).map(|n| sticker(&n.to_string(), if n == 2 { "😮" } else { "👋" }, "Forest Friends")).chain([sticker("x", "🦊", "Foxes")]).collect();
        let lines = sticker_lines(&items, "", None, &[], &[]);
        assert_eq!(lines[0].label.as_deref(), Some("Forest Friends · 6"));
        assert_eq!(lines[1].items.len(), 4);
        assert_eq!(lines[2].items.len(), 2);
        assert_eq!(lines[3].label.as_deref(), Some("Foxes · 1"));
        let only = sticker_lines(&items, "", Some("Foxes"), &[], &[]);
        assert_eq!(only.len(), 2);
        let found = sticker_lines(&items, "😮", None, &[], &[]);
        assert_eq!(found[1].items, [(2, STICKER)]);
        // Recent and Favorites lead with nothing typed, and only then.
        let lines = sticker_lines(&items, "", None, &["x".to_owned()], &["2".to_owned(), "gone".to_owned()]);
        assert_eq!(lines[0].label.as_deref(), Some("Recent"));
        assert_eq!(lines[1].items, [(2, STICKER)]);
        assert_eq!(lines[2].label.as_deref(), Some("Favorites"));
        assert_eq!(lines[3].items, [(6, STICKER)]);
        assert_eq!(lines[4].label.as_deref(), Some("Forest Friends · 6"));
        assert!(sticker_lines(&items, "", Some("Foxes"), &["x".to_owned()], &["2".to_owned()])[0].label.as_deref() == Some("Foxes · 1"));
    }
}
