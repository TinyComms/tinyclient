//! A long list held as the few pages near the viewport. chatd holds the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Rows asked for at a time: about three screens of two-line rows.
pub const PAGE: u32 = 50;

pub struct Pages<T> {
    /// How many rows the list has in all, as chatd last said.
    pub total: u32,
    held: BTreeMap<u32, Vec<T>>,
    asked: BTreeSet<u32>,
    /// Rises whenever what is held stops being current; an answer from before is dropped.
    epoch: u64,
    /// Nothing has been answered yet, so the list cannot say it is empty.
    pub fresh: bool,
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Pages { total: 0, held: BTreeMap::new(), asked: BTreeSet::new(), epoch: 0, fresh: true }
    }
}

impl<T> Pages<T> {
    pub fn get(&self, row: usize) -> Option<&T> {
        let row = row as u32;
        self.held.get(&(row / PAGE))?.get((row % PAGE) as usize)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.held.values().map(Vec::len).sum()
    }

    /// The rows that are held, in order, with their position in the list.
    pub fn rows(&self) -> impl Iterator<Item = (usize, &T)> {
        self.held.iter().flat_map(|(page, rows)| {
            rows.iter().enumerate().map(move |(ix, row)| ((page * PAGE) as usize + ix, row))
        })
    }

    fn near(visible: &Range<usize>) -> Range<u32> {
        let first = (visible.start as u32 / PAGE).saturating_sub(1);
        let last = visible.end.saturating_sub(1) as u32 / PAGE + 1;
        first..last + 1
    }

    /// The rows `visible` are on screen: drops the pages far from them and answers with the
    /// pages to ask for, one before and one after what shows so a scroll lands on rows.
    pub fn show(&mut self, visible: Range<usize>) -> Vec<u32> {
        let near = Self::near(&visible);
        self.held.retain(|page, _| near.contains(page));
        self.asked.retain(|page| near.contains(page));
        let pages = self.total.div_ceil(PAGE).max(1);
        let wanted: Vec<u32> = near
            .filter(|page| *page < pages && !self.held.contains_key(page) && !self.asked.contains(page))
            .collect();
        self.asked.extend(&wanted);
        wanted
    }

    /// Changes the held rows that match, where they are.
    pub fn replace(&mut self, matches: impl Fn(&T) -> bool, with: &T)
    where
        T: Clone,
    {
        for row in self.held.values_mut().flatten().filter(|row| matches(row)) {
            *row = with.clone();
        }
    }

    /// What is held is stale. It stays drawn until its replacement lands; answers with the pages
    /// to read again.
    pub fn stale(&mut self) -> Vec<u32> {
        self.epoch += 1;
        let mut pages: Vec<u32> = self.held.keys().copied().collect();
        if pages.is_empty() {
            pages.push(0);
        }
        self.asked = pages.iter().copied().collect();
        pages
    }

    /// The list is a different one now (another filter): nothing held is of any use.
    pub fn reset(&mut self) {
        self.epoch += 1;
        self.held.clear();
        self.asked = BTreeSet::from([0]);
        self.total = 0;
        self.fresh = true;
    }

    /// An answer from chatd. False when it was asked before the list last went stale.
    pub fn fill(&mut self, epoch: u64, page: u32, rows: Vec<T>, total: u32) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.asked.remove(&page);
        self.total = total;
        self.fresh = false;
        // The list shrank under us: pages past its end hold nothing now.
        let pages = total.div_ceil(PAGE).max(1);
        self.held.retain(|held, _| *held < pages);
        if page < pages {
            self.held.insert(page, rows);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(total: u32, pages: &[u32]) -> Pages<u32> {
        let mut list = Pages::default();
        for page in pages {
            let rows = (page * PAGE..((page + 1) * PAGE).min(total)).collect();
            assert!(list.fill(0, *page, rows, total));
        }
        list
    }

    #[test]
    fn scrolling_asks_for_the_pages_around_the_viewport_and_drops_the_far_ones() {
        let mut list = filled(1000, &[0]);
        assert_eq!(list.show(0..14), [1]);
        // Asked once: a second frame of the same rows asks for nothing.
        assert_eq!(list.show(0..14), Vec::<u32>::new());

        // A jump far down: the first page is dropped, the three around the viewport are wanted.
        assert_eq!(list.show(510..524), [9, 10, 11]);
        assert_eq!(list.held(), 0);
        for page in [9, 10, 11] {
            list.fill(0, page, (page * PAGE..(page + 1) * PAGE).collect(), 1000);
        }
        assert_eq!(list.get(510), Some(&510));
        assert_eq!(list.get(10), None);
        assert_eq!(list.held(), 150, "never more than the pages near the viewport");
    }

    #[test]
    fn the_last_page_is_short_and_nothing_is_asked_past_the_end() {
        let mut list = filled(120, &[0]);
        assert_eq!(list.show(40..60), [1, 2]);
        list.fill(0, 1, (50..100).collect(), 120);
        list.fill(0, 2, (100..120).collect(), 120);
        assert_eq!(list.get(119), Some(&119));
        assert_eq!(list.get(120), None);
        assert_eq!(list.show(100..120), Vec::<u32>::new());
    }

    #[test]
    fn a_stale_list_keeps_its_rows_until_the_new_ones_land_and_drops_late_answers() {
        let mut list = filled(60, &[0, 1]);
        let before = list.epoch();
        assert_eq!(list.stale(), [0, 1]);
        assert_eq!(list.get(3), Some(&3), "still drawn while the read is out");
        assert!(!list.fill(before, 0, vec![99], 1), "an answer from before the change");
        assert_eq!(list.total, 60);

        // The new answer says the list shrank to one page: the second page goes.
        assert!(list.fill(list.epoch(), 0, vec![7, 8], 2));
        assert_eq!((list.get(0), list.get(1), list.get(50)), (Some(&7), Some(&8), None));
        assert_eq!(list.total, 2);
    }

    #[test]
    fn another_filter_starts_from_nothing() {
        let mut list = filled(200, &[0, 1]);
        list.reset();
        assert!(list.fresh);
        assert_eq!((list.held(), list.total), (0, 0));
        assert!(list.show(0..10).is_empty(), "the first page is already on its way");
    }
}
