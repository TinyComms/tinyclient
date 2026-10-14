//! The messages held for the open conversation: one unbroken run around what is on screen.
//! History beyond it stays in chatd and is asked for a page at a time.

use protocol::{MediaFile, Message, Page};
use std::collections::HashSet;
use std::rc::Rc;

/// Most messages held. Scrolling further drops the far end.
pub const WINDOW: usize = 300;
/// Messages asked for at a time.
pub const PAGE: u32 = 60;

#[derive(Default)]
pub struct Window {
    /// Oldest first, with nothing missing in between.
    pub messages: Vec<Rc<Message>>,
    /// chatd holds messages older than the first.
    pub older: bool,
    /// chatd holds messages newer than the last.
    pub newer: bool,
}

/// chatd's own order: time, then id as a number would sort.
type Order = (i64, usize, String);

fn order(message: &Message) -> Order {
    (message.sent, message.id.len(), message.id.clone())
}

/// What an answer says about a stretch of the conversation: everything between `from` and `to`
/// is in it. `None` is open-ended.
struct Covered {
    from: Option<(Order, bool)>,
    to: Option<(Order, bool)>,
}

impl Covered {
    fn before(&self, at: &Order) -> bool {
        self.from.as_ref().is_some_and(|(from, inclusive)| at < from || (!inclusive && at == from))
    }

    fn after(&self, at: &Order) -> bool {
        self.to.as_ref().is_some_and(|(to, inclusive)| at > to || (!inclusive && at == to))
    }
}

impl Window {
    pub fn position(&self, id: &str) -> Option<usize> {
        self.messages.iter().position(|message| message.id == id)
    }

    /// Takes chatd's answer to `page`. What the answer covers replaces what was held there, so
    /// edits, deletions and arrivals all land; the rest stays. An answer about somewhere the
    /// window does not reach replaces the window. Returns false when nothing changed.
    pub fn merge(&mut self, page: &Page, found: Vec<Message>, older: bool, newer: bool) -> bool {
        let first = found.first().map(order);
        let last = found.last().map(order);
        let held = |id: &str| self.messages.iter().find(|message| message.id == id).map(|m| order(m));
        let covered = match page {
            Page::Newest => Covered { from: first.map(|first| (first, true)), to: None },
            Page::Before { id } => {
                let Some(anchor) = held(id) else { return false };
                let from = first.filter(|_| older).map(|first| (first, true));
                Covered { from, to: Some((anchor, false)) }
            }
            Page::After { id } => {
                let Some(anchor) = held(id) else { return false };
                let to = last.filter(|_| newer).map(|last| (last, true));
                Covered { from: Some((anchor, false)), to }
            }
            Page::Around { .. } => {
                if found.is_empty() {
                    return false;
                }
                Covered {
                    from: first.filter(|_| older).map(|first| (first, true)),
                    to: last.filter(|_| newer).map(|last| (last, true)),
                }
            }
        };

        let before = self.messages.iter().take_while(|message| covered.before(&order(message))).count();
        let after = self.messages.iter().rev().take_while(|message| covered.after(&order(message))).count();
        // Everything held is on one side of the answer: there may be a gap between them.
        let apart = !self.messages.is_empty()
            && (before == self.messages.len() || after == self.messages.len())
            && !matches!(page, Page::Before { .. } | Page::After { .. });

        let mut next = Vec::with_capacity(found.len() + before + after);
        if !apart {
            next.extend(self.messages[..before].iter().cloned());
        }
        // An answer may reach a row past what it says it covers (201 rows for a limit of 200,
        // or the same row twice): the answer's copy is the one kept, once.
        let mut answered: HashSet<String> = HashSet::with_capacity(found.len());
        next.retain(|held: &Rc<Message>| !found.iter().any(|message| message.id == held.id));
        for message in found {
            if !answered.insert(message.id.clone()) {
                continue;
            }
            // The same message keeps its allocation, so a row can tell cheaply that it did not change.
            let same = self.messages.iter().find(|held| ***held == message);
            next.push(same.cloned().unwrap_or_else(|| Rc::new(message)));
        }
        let kept_before = !apart && before > 0;
        let kept_after = !apart && after > 0;
        if kept_after {
            let tail = self.messages[self.messages.len() - after..].iter();
            next.extend(tail.filter(|held| !answered.contains(&held.id)).cloned());
        }

        let next_older = if kept_before { self.older } else { older };
        let next_newer = if kept_after { self.newer } else { newer };
        let unchanged = next_older == self.older
            && next_newer == self.newer
            && next.len() == self.messages.len()
            && next.iter().zip(&self.messages).all(|(a, b)| Rc::ptr_eq(a, b));
        self.messages = next;
        self.older = next_older;
        self.newer = next_newer;
        !unchanged
    }

    /// Brings the window back under [`WINDOW`] by dropping the end farther from `keep`.
    pub fn trim(&mut self, keep: usize) {
        let extra = self.messages.len().saturating_sub(WINDOW);
        if extra == 0 {
            return;
        }
        if keep >= self.messages.len() / 2 {
            self.messages.drain(..extra);
            self.older = true;
        } else {
            self.messages.truncate(WINDOW);
            self.newer = true;
        }
    }

    /// A file landed on disk, or could not be fetched: every use of it takes the new value.
    pub fn landed(&mut self, file: &MediaFile) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            if !message.files().any(|held| held.id == file.id && differs(held, file)) {
                continue;
            }
            for held in Rc::make_mut(message).files_mut().filter(|held| held.id == file.id) {
                take(held, file);
            }
            changed = true;
        }
        changed
    }
}

/// Which of the files the rows want must be asked for, by size: those never asked for, and
/// those now shown larger than they were asked for at. `asked` remembers the largest.
pub fn unasked(want: Vec<(String, u32)>, asked: &mut std::collections::HashMap<String, u32>) -> std::collections::BTreeMap<u32, Vec<String>> {
    let mut by_size: std::collections::BTreeMap<u32, Vec<String>> = std::collections::BTreeMap::new();
    let mut largest: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (id, size) in want {
        let most = largest.entry(id).or_default();
        *most = (*most).max(size);
    }
    for (id, size) in largest {
        if asked.get(&id).is_none_or(|before| *before < size) {
            asked.insert(id.clone(), size);
            by_size.entry(size).or_default().push(id);
        }
    }
    for ids in by_size.values_mut() {
        ids.sort_unstable();
    }
    by_size
}

/// Whether what chatd now says about a file is news to the copy a message holds.
pub fn differs(held: &MediaFile, file: &MediaFile) -> bool {
    (file.path.is_some() && held.path != file.path) || held.failed != file.failed
}

/// Takes what chatd now says about a file. A file that is being fetched again at a larger
/// size is answered with no path while the one on disk is still good, so a path is only ever
/// replaced by another.
pub fn take(held: &mut MediaFile, file: &MediaFile) {
    if file.path.is_some() {
        held.path.clone_from(&file.path);
    }
    held.failed.clone_from(&file.failed);
    if file.bytes != 0 {
        held.bytes = file.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn said(id: u32) -> Message {
        Message::plain(id.to_string(), "mara", format!("message {id}"), i64::from(id) * 10, false)
    }

    fn run(ids: std::ops::RangeInclusive<u32>) -> Vec<Message> {
        ids.map(said).collect()
    }

    fn ids(window: &Window) -> Vec<u32> {
        window.messages.iter().map(|message| message.id.parse().unwrap()).collect()
    }

    fn before(id: u32) -> Page {
        Page::Before { id: id.to_string() }
    }

    #[test]
    fn older_pages_join_the_front_and_the_far_end_is_dropped_past_the_limit() {
        let mut window = Window::default();
        assert!(window.merge(&Page::Newest, run(941..=1000), true, false));
        assert_eq!((window.messages.len(), window.older, window.newer), (60, true, false));

        for oldest in [941, 881, 821, 761] {
            assert!(window.merge(&before(oldest), run(oldest - 60..=oldest - 1), true, true));
            window.trim(0);
        }
        // 300 held: the four older pages and the first one. A fifth pushes the newest out.
        assert_eq!((ids(&window)[0], *ids(&window).last().unwrap(), window.newer), (701, 1000, false));
        assert!(window.merge(&before(701), run(641..=700), true, true));
        window.trim(0);
        assert_eq!(window.messages.len(), WINDOW);
        assert_eq!((ids(&window)[0], *ids(&window).last().unwrap()), (641, 940));
        assert!(window.older && window.newer);

        // Coming back down: the page after the last held joins the end and the front goes.
        let after = Page::After { id: "940".into() };
        assert!(window.merge(&after, run(941..=1000), true, false));
        window.trim(window.messages.len() - 1);
        assert_eq!((ids(&window)[0], *ids(&window).last().unwrap()), (701, 1000));
        assert!(window.older && !window.newer);
    }

    #[test]
    fn the_start_of_history_is_known_when_an_older_page_says_so() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(3..=5), true, false);
        assert!(window.merge(&before(3), run(1..=2), false, true));
        assert_eq!((ids(&window), window.older), (vec![1, 2, 3, 4, 5], false));
    }

    #[test]
    fn a_refresh_lands_an_edit_a_deletion_and_an_arrival_and_leaves_the_rest_alone() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(1..=6), false, false);
        let untouched = window.messages[0].clone();

        let mut again = run(1..=7);
        again[2].text = "edited".into();
        again[2].edited = true;
        again.remove(4);
        assert!(window.merge(&Page::Newest, again.clone(), false, false));
        assert_eq!(ids(&window), [1, 2, 3, 4, 6, 7]);
        assert_eq!(window.messages[2].text, "edited");
        assert!(Rc::ptr_eq(&window.messages[0], &untouched));

        assert!(!window.merge(&Page::Newest, again, false, false), "the same answer changes nothing");
    }

    #[test]
    fn a_refresh_around_the_viewport_replaces_only_what_it_covers() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(1..=20), false, false);
        let mut middle = run(8..=12);
        middle.remove(2);
        let around = Page::Around { id: "10".into() };
        assert!(window.merge(&around, middle, true, true));
        assert_eq!(ids(&window), [1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]);
        assert!(!window.older && !window.newer);
    }

    #[test]
    fn a_page_far_from_the_window_replaces_it() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(900..=1000), true, false);
        let around = Page::Around { id: "100".into() };
        assert!(window.merge(&around, run(70..=130), true, true));
        assert_eq!((ids(&window)[0], window.messages.len()), (70, 61));
        assert!(window.older && window.newer);
    }

    #[test]
    fn new_messages_join_the_end_while_scrolled_up() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(1..=5), false, false);
        let after = Page::After { id: "5".into() };
        assert!(window.merge(&after, run(6..=7), true, false));
        assert_eq!(ids(&window), [1, 2, 3, 4, 5, 6, 7]);
        let nothing = Page::After { id: "7".into() };
        assert!(!window.merge(&nothing, Vec::new(), true, false));
        assert_eq!(window.messages.len(), 7);
    }

    #[test]
    fn a_page_that_overruns_its_limit_or_repeats_a_row_adds_no_duplicate() {
        let mut window = Window::default();
        window.merge(&Page::Newest, run(101..=300), true, false);
        // `Around` for 200 answered 201 rows, and said nothing newer is left though 300 is held.
        let around = Page::Around { id: "200".into() };
        assert!(window.merge(&around, run(100..=300), true, true));
        let held = ids(&window);
        assert_eq!((held.len(), held[0], held[200]), (201, 100, 300));
        let unique: HashSet<u32> = held.iter().copied().collect();
        assert_eq!(unique.len(), held.len());

        // An older page whose last row is the one it was asked to stop before.
        let mut window = Window::default();
        window.merge(&Page::Newest, run(50..=60), true, false);
        window.merge(&before(50), run(40..=50), true, true);
        assert_eq!(ids(&window), (40..=60).collect::<Vec<_>>());
        // The same row twice in one answer.
        let mut twice = run(61..=62);
        twice.push(said(62));
        window.merge(&Page::After { id: "60".into() }, twice, true, false);
        assert_eq!(ids(&window), (40..=62).collect::<Vec<_>>());
    }

    #[test]
    fn a_file_is_asked_for_once_and_again_only_when_it_is_shown_larger() {
        let mut asked = std::collections::HashMap::new();
        let want = |pairs: &[(&str, u32)]| pairs.iter().map(|(id, size)| ((*id).to_owned(), *size)).collect::<Vec<_>>();
        // The same file in a tile and as a reply's avatar: one ask, at the larger size.
        let first = unasked(want(&[("a", 396), ("a", 32), ("b", 800), ("c", 396)]), &mut asked);
        let sizes: Vec<(u32, Vec<String>)> = first.into_iter().collect();
        assert_eq!(sizes, [(396, vec!["a".to_owned(), "c".to_owned()]), (800, vec!["b".to_owned()])]);
        // Drawn again the same, or smaller: nothing.
        assert!(unasked(want(&[("a", 396), ("b", 400)]), &mut asked).is_empty());
        // The pane widened, or the window moved to a sharper screen.
        let again: Vec<_> = unasked(want(&[("a", 528)]), &mut asked).into_iter().collect();
        assert_eq!(again, [(528, vec!["a".to_owned()])]);
    }

    #[test]
    fn a_larger_size_on_its_way_keeps_the_picture_and_a_failure_is_remembered() {
        let mut window = Window::default();
        let mut with = said(1);
        let mut small = MediaFile::new("p", MediaKind::Image);
        small.path = Some("/tmp/p".into());
        with.attachments.push(protocol::Attachment::new("p.png", small));
        window.merge(&Page::Newest, vec![with], false, false);

        // Asked for again at a larger size: no path in the answer, the small one stays.
        assert!(!window.landed(&MediaFile::new("p", MediaKind::Image)));
        assert_eq!(window.messages[0].attachments[0].file.path.as_deref(), Some("/tmp/p"));

        let mut failed = MediaFile::new("p", MediaKind::Image);
        failed.failed = Some("The link expired.".into());
        assert!(window.landed(&failed));
        assert_eq!(window.messages[0].attachments[0].file.failed.as_deref(), Some("The link expired."));
    }

    #[test]
    fn a_file_landing_fills_in_its_path_everywhere_it_is_used() {
        let mut window = Window::default();
        let mut with = said(1);
        with.sender.avatar = Some(MediaFile::new("av", MediaKind::Avatar));
        let mut also = said(2);
        also.sender.avatar = Some(MediaFile::new("av", MediaKind::Avatar));
        window.merge(&Page::Newest, vec![with, also, said(3)], false, false);
        let plain = window.messages[2].clone();

        let mut landed = MediaFile::new("av", MediaKind::Avatar);
        landed.path = Some("/tmp/av.png".into());
        assert!(window.landed(&landed));
        for message in &window.messages[..2] {
            assert_eq!(message.sender.avatar.as_ref().unwrap().path.as_deref(), Some("/tmp/av.png"));
        }
        assert!(Rc::ptr_eq(&window.messages[2], &plain));
        assert!(!window.landed(&landed));
    }
}
