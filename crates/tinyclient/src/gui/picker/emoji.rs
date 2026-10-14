//! The emoji tab's data: the Unicode table compiled in, custom emoji from the network, skin
//! tone, search, and the "Frequently used" row kept per account. No drawing here.

use super::table::{EMOJI, GROUPS};
use protocol::{AccountKey, MediaFile, Network};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

pub const COLUMNS: usize = 9;
/// Kept per account; the first two rows are shown.
pub const KEPT: usize = 36;
pub const SHOWN: usize = 18;

/// One Unicode emoji. `code` is its shortcode without colons; `words` are what search reads.
#[derive(Debug)]
pub struct Emoji {
    pub glyph: &'static str,
    pub group: u8,
    /// Takes a skin tone modifier after its first code point.
    pub tones: bool,
    pub code: &'static str,
    pub words: &'static str,
}

/// The five modifiers after "no tone", for the footer's swatches.
pub const TONES: [&str; 6] = ["", "\u{1F3FB}", "\u{1F3FC}", "\u{1F3FD}", "\u{1F3FE}", "\u{1F3FF}"];

/// The glyph in a skin tone: the modifier follows the first code point, in place of the
/// presentation selector that may follow it.
pub fn toned(emoji: &Emoji, tone: usize) -> String {
    let modifier = TONES.get(tone).copied().unwrap_or("");
    if !emoji.tones || modifier.is_empty() {
        return emoji.glyph.to_owned();
    }
    let mut chars = emoji.glyph.chars();
    let first = chars.next().map(String::from).unwrap_or_default();
    let rest: String = chars.as_str().strip_prefix('\u{FE0F}').unwrap_or(chars.as_str()).to_owned();
    format!("{first}{modifier}{rest}")
}

/// A network's own emoji, by `:name:`.
#[derive(Debug, Clone, PartialEq)]
pub struct Custom {
    pub name: String,
    /// What the network calls it (`CatalogItem::id`); empty for one seen on a message.
    pub id: String,
    pub file: Option<MediaFile>,
    /// The Space it belongs to, for its section.
    pub pack: String,
    pub animated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// An index into the table.
    Unicode(u16),
    /// Used before, and not in the table (an emoji of a newer Unicode than ours).
    Glyph(String),
    /// An index into the customs.
    Custom(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Label(String),
    Cells(Vec<Cell>),
}

/// What a cell puts in the field or reacts with, in the tone chosen.
pub fn text_of(cell: &Cell, tone: usize, customs: &[Custom]) -> String {
    match cell {
        Cell::Unicode(ix) => EMOJI.get(*ix as usize).map_or_else(String::new, |emoji| toned(emoji, tone)),
        Cell::Glyph(glyph) => glyph.clone(),
        Cell::Custom(ix) => customs.get(*ix as usize).map_or_else(String::new, |custom| custom.name.clone()),
    }
}

/// The `:name:` the footer shows for a cell.
pub fn name_of(cell: &Cell, customs: &[Custom]) -> String {
    match cell {
        Cell::Unicode(ix) => EMOJI.get(*ix as usize).map_or_else(String::new, |emoji| format!(":{}:", emoji.code)),
        Cell::Glyph(glyph) => find(glyph).map_or_else(String::new, |ix| format!(":{}:", EMOJI[ix].code)),
        Cell::Custom(ix) => customs.get(*ix as usize).map_or_else(String::new, |custom| custom.name.clone()),
    }
}

/// The table entry for a glyph, in any tone.
pub fn find(glyph: &str) -> Option<usize> {
    let bare = |s: &'static str| s.chars().filter(|ch| *ch != '\u{FE0F}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(ch));
    let wanted: Vec<char> = glyph.chars().filter(|ch| *ch != '\u{FE0F}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(ch)).collect();
    EMOJI.iter().position(|emoji| bare(emoji.glyph).eq(wanted.iter().copied()))
}

fn chunked(rows: &mut Vec<Row>, cells: Vec<Cell>) {
    for chunk in cells.chunks(COLUMNS) {
        rows.push(Row::Cells(chunk.to_vec()));
    }
}

/// Where each section starts in the rows, for the strip: (label, row).
pub type Sections = Vec<(String, usize)>;

/// The grid with no query: frequently used, the customs by their Space, then the Unicode
/// groups.
pub fn browse(recent: &[String], customs: &[Custom]) -> (Vec<Row>, Sections) {
    let mut rows = Vec::new();
    let mut sections = Vec::new();
    let recent: Vec<Cell> = recent
        .iter()
        .take(SHOWN)
        .filter_map(|used| match used.starts_with(':') {
            // A custom emoji is offered only where its image is known: from this account.
            true => customs.iter().position(|custom| custom.name == *used).map(|ix| Cell::Custom(ix as u32)),
            false => Some(match find(used) {
                Some(ix) if EMOJI[ix].glyph == used => Cell::Unicode(ix as u16),
                _ => Cell::Glyph(used.clone()),
            }),
        })
        .collect();
    if !recent.is_empty() {
        sections.push(("Frequently used".to_owned(), rows.len()));
        rows.push(Row::Label("Frequently used".to_owned()));
        chunked(&mut rows, recent);
    }
    let mut packs: Vec<&str> = Vec::new();
    for custom in customs {
        if !packs.contains(&custom.pack.as_str()) {
            packs.push(&custom.pack);
        }
    }
    for pack in packs {
        let label = if pack.is_empty() { "Custom".to_owned() } else { pack.to_owned() };
        sections.push((label.clone(), rows.len()));
        rows.push(Row::Label(label));
        let cells = customs.iter().enumerate().filter(|(_, custom)| custom.pack == pack).map(|(ix, _)| Cell::Custom(ix as u32)).collect();
        chunked(&mut rows, cells);
    }
    for (group, label) in GROUPS.iter().enumerate() {
        sections.push(((*label).to_owned(), rows.len()));
        rows.push(Row::Label((*label).to_owned()));
        let cells = EMOJI.iter().enumerate().filter(|(_, emoji)| emoji.group as usize == group).map(|(ix, _)| Cell::Unicode(ix as u16)).collect();
        chunked(&mut rows, cells);
    }
    (rows, sections)
}

/// How well an emoji's words answer a query: 0 its code is the query, 1 its code starts with
/// it, 2 a word does, 3 a word contains it.
pub fn score(code: &str, words: &str, query: &str) -> Option<u8> {
    if code == query {
        Some(0)
    } else if code.starts_with(query) {
        Some(1)
    } else if words.split(' ').any(|word| word.starts_with(query)) {
        Some(2)
    } else if words.contains(query) {
        Some(3)
    } else {
        None
    }
}

/// Every match for a query, best first: by score, then used most, then the table's order.
/// Customs rank with the rest.
pub fn search(query: &str, recent: &[String], customs: &[Custom], limit: usize) -> Vec<Cell> {
    let query = query.trim().trim_matches(':').to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let used = |text: &str| recent.iter().position(|held| held == text).unwrap_or(usize::MAX);
    let mut found: Vec<(u8, usize, usize, Cell)> = Vec::new();
    for (ix, custom) in customs.iter().enumerate() {
        let name = custom.name.trim_matches(':').to_lowercase();
        if let Some(score) = score(&name, &name.replace('_', " "), &query) {
            found.push((score, used(&custom.name), ix, Cell::Custom(ix as u32)));
        }
    }
    for (ix, emoji) in EMOJI.iter().enumerate() {
        if let Some(score) = score(emoji.code, emoji.words, &query) {
            found.push((score, used(emoji.glyph), customs.len() + ix, Cell::Unicode(ix as u16)));
        }
    }
    found.sort_by_key(|(score, used, order, _)| (*score, *used, *order));
    found.into_iter().take(limit).map(|(.., cell)| cell).collect()
}

pub fn search_rows(query: &str, recent: &[String], customs: &[Custom]) -> Vec<Row> {
    let mut rows = Vec::new();
    chunked(&mut rows, search(query, recent, customs, usize::MAX));
    rows
}

// Frequently used, per account.

/// Each account's emoji, by a count that halves in weight every few dozen uses, so what was
/// used a lot long ago gives way to what is used now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frecency {
    used: Vec<(String, f32)>,
}

impl Frecency {
    const DECAY: f32 = 0.95;

    pub fn bump(&mut self, emoji: &str) {
        for (_, weight) in &mut self.used {
            *weight *= Self::DECAY;
        }
        match self.used.iter_mut().find(|(held, _)| held == emoji) {
            Some((_, weight)) => *weight += 1.,
            None => self.used.push((emoji.to_owned(), 1.)),
        }
        // Stable: equal weights keep the newer first.
        self.used.sort_by(|a, b| b.1.total_cmp(&a.1));
        self.used.truncate(KEPT);
    }

    pub fn list(&self) -> Vec<String> {
        self.used.iter().map(|(emoji, _)| emoji.clone()).collect()
    }
}

/// What the reaction toolbar and the empty picker offer before anything is used.
const DEFAULTS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "🙏", "👀"];

pub fn filled(used: &[String], count: usize) -> Vec<String> {
    let mut out: Vec<String> = used.iter().filter(|held| !held.starts_with(':')).take(count).cloned().collect();
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

pub(super) fn account_word(account: &AccountKey) -> String {
    let network = match account.network {
        Network::Discord => "discord",
        Network::Slack => "slack",
        Network::Telegram => "telegram",
        Network::Teams => "teams",
    };
    format!("{network}:{}", account.account)
}

/// The file: one line per account, `account\ttone\temoji weight\temoji weight…`.
pub fn parse(text: &str) -> HashMap<String, (usize, Frecency)> {
    let mut all = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(account), Some(tone)) = (fields.next(), fields.next()) else {
            continue;
        };
        let tone = tone.parse::<usize>().unwrap_or(0).min(TONES.len() - 1);
        let used = fields
            .filter_map(|field| {
                let (emoji, weight) = field.rsplit_once(' ')?;
                Some((emoji.to_owned(), weight.parse::<f32>().ok()?))
            })
            .take(KEPT)
            .collect();
        all.insert(account.to_owned(), (tone, Frecency { used }));
    }
    all
}

pub fn write(all: &HashMap<String, (usize, Frecency)>) -> String {
    let mut accounts: Vec<&String> = all.keys().collect();
    accounts.sort();
    let mut out = String::new();
    for account in accounts {
        let (tone, frecency) = &all[account];
        out.push_str(&format!("{account}\t{tone}"));
        for (emoji, weight) in &frecency.used {
            out.push_str(&format!("\t{emoji} {weight:.3}"));
        }
        out.push('\n');
    }
    out
}

// CONTRACT: picker recents belong in SQLite per account (architecture, "left for later"); until
// chatd stores them they are a small file of the shell's beside chatd's, read once and written
// off the main thread after each use.
const FILE: &str = "picker.recents";

struct Recents {
    all: HashMap<String, (usize, Frecency)>,
    /// The account the open conversation is on: reactions and the picker use its row.
    current: Option<String>,
    path: Option<PathBuf>,
}

thread_local! {
    static RECENTS: RefCell<Option<Recents>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut Recents) -> T) -> T {
    RECENTS.with_borrow_mut(|slot| {
        let recents = slot.get_or_insert_with(|| {
            let path = (!cfg!(test)).then(|| protocol::support_dir().join(FILE));
            let text = path.as_ref().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default();
            Recents { all: parse(&text), current: None, path }
        });
        f(recents)
    })
}

/// The account whose row the next pick goes to: the open conversation's.
pub fn set_account(account: Option<&AccountKey>) {
    with(|recents| recents.current = account.map(account_word));
}

/// The current account's emoji, most used first.
pub fn recent() -> Vec<String> {
    with(|recents| recents.current.as_ref().and_then(|account| recents.all.get(account)).map(|(_, frecency)| frecency.list()).unwrap_or_default())
}

pub fn tone() -> usize {
    with(|recents| recents.current.as_ref().and_then(|account| recents.all.get(account)).map_or(0, |(tone, _)| *tone))
}

pub fn set_tone(tone: usize) {
    with(|recents| {
        let Some(account) = recents.current.clone() else {
            return;
        };
        recents.all.entry(account).or_default().0 = tone.min(TONES.len() - 1);
        save(recents);
    });
}

/// An emoji was sent or reacted with.
pub fn used(emoji: &str) {
    with(|recents| {
        let Some(account) = recents.current.clone() else {
            return;
        };
        recents.all.entry(account).or_default().1.bump(emoji);
        save(recents);
    });
}

fn save(recents: &Recents) {
    let Some(path) = recents.path.clone() else {
        return;
    };
    let text = write(&recents.all);
    // Small and rare (one per pick); a thread keeps the disk off the frame.
    let _ = std::thread::Builder::new().name("picker-recents".into()).stack_size(64 * 1024).spawn(move || {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyphs(cells: &[Cell], customs: &[Custom]) -> Vec<String> {
        cells.iter().map(|cell| text_of(cell, 0, customs)).collect()
    }

    fn custom(name: &str, pack: &str) -> Custom {
        Custom { name: name.into(), id: format!("id-{name}"), file: None, pack: pack.into(), animated: false }
    }

    #[test]
    fn the_table_is_the_whole_unicode_set_with_shortcodes() {
        assert!(EMOJI.len() > 1_850, "{}", EMOJI.len());
        assert_eq!(GROUPS.len(), 8);
        let tada = &EMOJI[find("🎉").expect("tada")];
        assert_eq!(tada.code, "tada");
        assert!(EMOJI.iter().all(|emoji| !emoji.glyph.is_empty() && !emoji.code.is_empty() && !emoji.code.contains(' ')));
        assert!(EMOJI.windows(2).all(|pair| pair[0].group <= pair[1].group), "grouped in order");
    }

    #[test]
    fn a_skin_tone_goes_after_the_first_code_point_and_only_where_it_applies() {
        let wave = &EMOJI[find("👋").unwrap()];
        assert_eq!(toned(wave, 3), "👋\u{1F3FD}");
        assert_eq!(toned(wave, 0), "👋");
        let raised = &EMOJI[find("☝️").unwrap()];
        assert!(raised.tones);
        assert_eq!(toned(raised, 1), "☝\u{1F3FB}", "the selector gives way to the modifier");
        let tada = &EMOJI[find("🎉").unwrap()];
        assert_eq!(toned(tada, 5), "🎉");
        assert_eq!(find("👋\u{1F3FD}"), find("👋"));
    }

    #[test]
    fn search_puts_the_exact_code_first_and_ranks_customs_with_the_rest() {
        let customs = [custom(":tada_marten:", "Moss & Lichen"), custom(":tadpole:", "Moss & Lichen")];
        let found = glyphs(&search("tada", &[], &customs, 4), &customs);
        assert_eq!(found[0], "🎉");
        assert_eq!(found[1], ":tada_marten:");
        assert!(glyphs(&search(":tad", &[], &customs, 8), &customs).contains(&":tadpole:".to_owned()));
        let thumbs = glyphs(&search("thumbsup", &[], &[], 1), &[]);
        assert_eq!(thumbs, ["👍"]);
        assert!(search("zzzzqqq", &[], &customs, 8).is_empty());
        assert!(search("  ", &[], &customs, 8).is_empty());
        // Of equal matches, the one used most leads.
        let plain = glyphs(&search("heart", &[], &[], 3), &[]);
        let used = glyphs(&search("heart", &["💜".into()], &[], 3), &[]);
        assert_ne!(plain[0], "💜");
        assert!(used.iter().position(|g| g == "💜") < plain.iter().position(|g| g == "💜").or(Some(usize::MAX)));
    }

    #[test]
    fn browsing_shows_frequent_then_the_spaces_emoji_then_every_group() {
        let customs = [custom(":mossparrot:", "Moss & Lichen")];
        let (rows, sections) = browse(&["🚀".into(), ":mossparrot:".into(), ":elsewhere:".into()], &customs);
        assert_eq!(rows[0], Row::Label("Frequently used".into()));
        let Row::Cells(first) = &rows[1] else { panic!("cells") };
        // A custom emoji with no image here is left out.
        assert_eq!(glyphs(first, &customs), ["🚀", ":mossparrot:"]);
        let labels: Vec<&str> = sections.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels[..3], ["Frequently used", "Moss & Lichen", "Smileys & people"]);
        assert_eq!(labels.len(), 2 + GROUPS.len());
        assert!(rows.iter().all(|row| !matches!(row, Row::Cells(cells) if cells.len() > COLUMNS)));
        let total: usize = rows.iter().map(|row| if let Row::Cells(cells) = row { cells.len() } else { 0 }).sum();
        assert_eq!(total, 2 + 1 + EMOJI.len());
        let (empty, sections) = browse(&[], &[]);
        assert_eq!(empty[0], Row::Label("Smileys & people".into()));
        assert_eq!(sections.len(), GROUPS.len());
    }

    #[test]
    fn frecency_favours_what_is_used_now_and_keeps_a_bounded_list() {
        let mut f = Frecency::default();
        for _ in 0..5 {
            f.bump("🔥");
        }
        f.bump("🚀");
        assert_eq!(f.list(), ["🔥", "🚀"]);
        for _ in 0..40 {
            f.bump("🚀");
        }
        assert_eq!(f.list()[0], "🚀");
        for n in 0..60 {
            f.bump(&n.to_string());
        }
        assert_eq!(f.list().len(), KEPT);
        assert_eq!(filled(&[], 3), ["👍", "❤️", "😂"]);
        assert_eq!(filled(&[":parrot:".into(), "🚀".into(), "👍".into()], 3), ["🚀", "👍", "❤️"]);
    }

    #[test]
    fn recents_survive_a_round_trip_through_their_file_per_account() {
        let mut all = HashMap::new();
        let mut f = Frecency::default();
        f.bump("🎉");
        f.bump(":mossparrot:");
        all.insert("discord:1".to_owned(), (3, f.clone()));
        all.insert("slack:T1".to_owned(), (0, Frecency::default()));
        let back = parse(&write(&all));
        assert_eq!(back["discord:1"].0, 3);
        assert_eq!(back["discord:1"].1.list(), f.list());
        assert!(back["slack:T1"].1.list().is_empty());
        assert!(parse("garbage\n\tno\n").get("garbage").is_none_or(|(tone, _)| *tone == 0));

        // The current account's row is the one used; another account's is apart.
        let discord = AccountKey { network: Network::Discord, account: "1".into() };
        let slack = AccountKey { network: Network::Slack, account: "T1".into() };
        set_account(Some(&discord));
        used("🦊");
        set_tone(2);
        set_account(Some(&slack));
        assert!(recent().is_empty());
        assert_eq!(tone(), 0);
        set_account(Some(&discord));
        assert_eq!(recent(), ["🦊"]);
        assert_eq!(tone(), 2);
    }
}
