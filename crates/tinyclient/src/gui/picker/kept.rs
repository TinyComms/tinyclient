//! What the GIF and Stickers tabs remember per account: the starred GIFs and stickers
//! ("Favorites") and the stickers sent last ("Recent"). A small file of the shell's beside the
//! emoji recents (`emoji.rs`), read once and written off the main thread after each change.
//!
//! // CONTRACT: the design puts favorites on the network's own list (Discord's frecency
//! // settings, Telegram's `saveGif` / `faveSticker`), which no request carries yet. Until one
//! // does, a star is kept here, so it is this Mac's and not the account's other clients'.

use super::emoji::account_word;
use protocol::AccountKey;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// Kept per account and list.
pub const KEPT: usize = 36;
/// Recent stickers shown, one row of four.
pub const RECENT_SHOWN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Favorites,
    RecentStickers,
}

impl List {
    fn word(self) -> &'static str {
        match self {
            List::Favorites => "favorites",
            List::RecentStickers => "recent-stickers",
        }
    }

    fn from_word(word: &str) -> Option<List> {
        match word {
            "favorites" => Some(List::Favorites),
            "recent-stickers" => Some(List::RecentStickers),
            _ => None,
        }
    }
}

type All = HashMap<(String, List), Vec<String>>;

/// The file: one line per account and list, `account\tlist\tid\tid…`, newest first.
pub fn parse(text: &str) -> All {
    let mut all = All::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(account), Some(list)) = (fields.next(), fields.next().and_then(List::from_word)) else {
            continue;
        };
        let ids = fields.filter(|id| !id.is_empty()).take(KEPT).map(str::to_owned).collect();
        all.insert((account.to_owned(), list), ids);
    }
    all
}

pub fn write(all: &All) -> String {
    let mut keys: Vec<&(String, List)> = all.keys().filter(|key| !all[*key].is_empty()).collect();
    keys.sort_by_key(|(account, list)| (account.clone(), list.word()));
    let mut out = String::new();
    for key in keys {
        out.push_str(&format!("{}\t{}", key.0, key.1.word()));
        for id in &all[key] {
            out.push('\t');
            out.push_str(id);
        }
        out.push('\n');
    }
    out
}

/// An id the file can hold: one field on one line.
fn storable(id: &str) -> bool {
    !id.is_empty() && !id.contains(['\t', '\n', '\r'])
}

/// Stars `id`, or takes its star away. Answers whether it is starred now.
pub fn toggle(ids: &mut Vec<String>, id: &str) -> bool {
    if let Some(at) = ids.iter().position(|held| held == id) {
        ids.remove(at);
        return false;
    }
    if !storable(id) {
        return false;
    }
    ids.insert(0, id.to_owned());
    ids.truncate(KEPT);
    true
}

/// `id` was sent: it goes first.
pub fn bump(ids: &mut Vec<String>, id: &str) {
    if !storable(id) {
        return;
    }
    ids.retain(|held| held != id);
    ids.insert(0, id.to_owned());
    ids.truncate(KEPT);
}

const FILE: &str = "picker.kept";

struct Kept {
    all: All,
    path: Option<PathBuf>,
}

thread_local! {
    static KEPT_LISTS: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut Kept) -> T) -> T {
    KEPT_LISTS.with_borrow_mut(|slot| {
        let kept = slot.get_or_insert_with(|| {
            let path = (!cfg!(test)).then(|| protocol::support_dir().join(FILE));
            let text = path.as_ref().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default();
            Kept { all: parse(&text), path }
        });
        f(kept)
    })
}

/// The account's list, newest first.
pub fn list(account: &AccountKey, list: List) -> Vec<String> {
    with(|kept| kept.all.get(&(account_word(account), list)).cloned().unwrap_or_default())
}

/// Stars or unstars a GIF or sticker; answers whether it is starred now.
pub fn star(account: &AccountKey, id: &str) -> bool {
    with(|kept| {
        let starred = toggle(kept.all.entry((account_word(account), List::Favorites)).or_default(), id);
        save(kept);
        starred
    })
}

/// A sticker was sent.
pub fn sent_sticker(account: &AccountKey, id: &str) {
    with(|kept| {
        bump(kept.all.entry((account_word(account), List::RecentStickers)).or_default(), id);
        save(kept);
    });
}

fn save(kept: &Kept) {
    let Some(path) = kept.path.clone() else {
        return;
    };
    let text = write(&kept.all);
    let _ = std::thread::Builder::new().name("picker-kept".into()).stack_size(64 * 1024).spawn(move || {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_toggles_newest_first_and_the_lists_survive_their_file() {
        let mut ids = Vec::new();
        assert!(toggle(&mut ids, "https://tenor.com/view/cat-1"));
        assert!(toggle(&mut ids, "sticker-fox-wave"));
        assert_eq!(ids, ["sticker-fox-wave", "https://tenor.com/view/cat-1"]);
        assert!(!toggle(&mut ids, "https://tenor.com/view/cat-1"));
        assert_eq!(ids, ["sticker-fox-wave"]);
        assert!(!toggle(&mut ids, "bad\tid"), "an id the file cannot hold is not starred");
        let mut recent = vec!["a".to_owned(), "b".to_owned()];
        bump(&mut recent, "b");
        assert_eq!(recent, ["b", "a"]);
        for n in 0..50 {
            bump(&mut recent, &n.to_string());
        }
        assert_eq!(recent.len(), KEPT);

        let mut all = All::new();
        all.insert(("discord:1".into(), List::Favorites), ids.clone());
        all.insert(("telegram:7".into(), List::RecentStickers), vec!["s1".into(), "s2".into()]);
        all.insert(("slack:T".into(), List::Favorites), Vec::new());
        let text = write(&all);
        assert_eq!(text.lines().count(), 2, "an empty list is not written");
        let back = parse(&text);
        assert_eq!(back.get(&("discord:1".into(), List::Favorites)), Some(&ids));
        assert_eq!(back.get(&("telegram:7".into(), List::RecentStickers)).map(Vec::len), Some(2));
        assert!(parse("junk\nx\tnope\ta\n").is_empty());
    }
}
