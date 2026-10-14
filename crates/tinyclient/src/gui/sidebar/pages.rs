//! The Inbox held whole. chatd cuts it at 30 days and the shell at [`HELD`] rows, so it is read
//! in a few pages and kept in one piece: day groups and the Apps fold need all of it.

/// Rows asked for at a time.
pub const PAGE: u32 = 250;
/// The most rows the Inbox holds.
pub const HELD: u32 = 500;

pub struct Pages<T> {
    /// How many rows the list has, as chatd last said, up to [`HELD`].
    pub total: u32,
    held: Vec<T>,
    /// Rises whenever what is held stops being current; an answer from before is dropped.
    epoch: u64,
    /// Nothing has been answered yet, so the list cannot say it is empty.
    pub fresh: bool,
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Pages { total: 0, held: Vec::new(), epoch: 0, fresh: true }
    }
}

impl<T> Pages<T> {
    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    pub fn get(&self, row: usize) -> Option<&T> {
        self.held.get(row)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn all(&self) -> &[T] {
        &self.held
    }

    /// The rows that are held, in order, with their position in the list.
    pub fn rows(&self) -> impl Iterator<Item = (usize, &T)> {
        self.held.iter().enumerate()
    }

    /// Changes the held rows that match, where they are.
    pub fn replace(&mut self, matches: impl Fn(&T) -> bool, with: &T)
    where
        T: Clone,
    {
        for row in self.held.iter_mut().filter(|row| matches(row)) {
            *row = with.clone();
        }
    }

    /// What is held is stale. It stays drawn until its replacement lands; answers the pages to
    /// read again, which is always the whole list from its start.
    pub fn stale(&mut self) -> Vec<u32> {
        self.epoch += 1;
        vec![0]
    }

    /// The list is a different one now (another filter): nothing held is of any use.
    pub fn reset(&mut self) {
        self.epoch += 1;
        self.held.clear();
        self.total = 0;
        self.fresh = true;
    }

    /// The whole list from chatd. False when it was asked before the list last went stale.
    pub fn fill(&mut self, epoch: u64, rows: Vec<T>, total: u32) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.total = total.min(rows.len() as u32);
        self.held = rows;
        self.fresh = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_list_keeps_its_rows_until_the_new_ones_land_and_drops_late_answers() {
        let mut list = Pages::default();
        assert!(list.fill(0, vec![1, 2, 3], 3));
        let before = list.epoch();
        assert_eq!(list.stale(), [0]);
        assert_eq!(list.get(2), Some(&3), "still drawn while the read is out");
        assert!(!list.fill(before, vec![9], 1), "an answer from before the change");
        assert!(list.fill(list.epoch(), vec![7, 8], 2));
        assert_eq!((list.get(0), list.get(2), list.total), (Some(&7), None, 2));
        list.reset();
        assert!(list.fresh && list.all().is_empty());
    }
}
