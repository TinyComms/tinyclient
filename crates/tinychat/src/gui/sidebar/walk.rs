//! The unread walk (`alt-shift-down` / `alt-shift-up`): the next thing unread, wherever it is.
//! Past the end of the list on show it continues into the next place that holds something
//! unread, and round again, until nothing unread is left.
//!
//! The places are stops in a fixed order: the Inbox (every direct conversation and mention),
//! then each Space in the rail's network order (its channels).

/// The stops to try after the one the walk is in, nearest first, going down or up and wrapping
/// round. `unread[i]` says whether stop `i` holds anything unread. The stop the walk is in
/// comes last: what is unread in it lies behind the cursor.
pub fn route(unread: &[bool], from: usize, down: bool) -> Vec<usize> {
    let count = unread.len();
    if count == 0 {
        return Vec::new();
    }
    let from = from.min(count - 1);
    (1..=count)
        .map(|step| if down { (from + step) % count } else { (from + count - step) % count })
        .filter(|stop| unread[*stop])
        .collect()
}

/// Which row of an unread list to open next. `at` is where the open conversation sits in the
/// list, when it is in it; `seen[i]` says row `i` was already visited on this walk (a row
/// stays unread when the window is not the active one). Down takes the first unvisited row
/// after `at`, up the last one before it; with no `at`, the first or last unvisited row.
pub fn step(seen: &[bool], at: Option<usize>, down: bool) -> Option<usize> {
    let fresh = |ix: &usize| !seen[*ix];
    match (at, down) {
        (Some(at), true) => (at + 1..seen.len()).find(fresh),
        (Some(at), false) => (0..at.min(seen.len())).rev().find(fresh),
        (None, true) => (0..seen.len()).find(fresh),
        (None, false) => (0..seen.len()).rev().find(fresh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_goes_on_into_the_next_place_with_unread_and_wraps_round() {
        // Inbox, then four Spaces across two networks; the walk is in Space 1.
        let unread = [true, false, true, false, true];
        assert_eq!(route(&unread, 1, true), [2, 4, 0]);
        assert_eq!(route(&unread, 1, false), [0, 4, 2]);
        // From the last stop, down wraps to the Inbox; the stop it is in is tried last.
        assert_eq!(route(&unread, 4, true), [0, 2, 4]);
        assert_eq!(route(&unread, 0, false), [4, 2, 0]);
    }

    #[test]
    fn nothing_unread_anywhere_ends_the_walk() {
        assert_eq!(route(&[false, false, false], 1, true), Vec::<usize>::new());
        assert_eq!(route(&[], 0, true), Vec::<usize>::new());
        // Only where the walk already is: that stop alone, for what lies behind the cursor.
        assert_eq!(route(&[false, true], 1, true), [1]);
    }

    #[test]
    fn a_row_already_visited_is_passed_over_so_the_walk_ends() {
        // Nothing is marked read (the window is not active): three unread rows, visited in turn.
        let mut seen = [false, false, false];
        let mut at = None;
        let mut order = Vec::new();
        while let Some(next) = step(&seen, at, true) {
            seen[next] = true;
            at = Some(next);
            order.push(next);
        }
        assert_eq!(order, [0, 1, 2]);
        assert_eq!(step(&[true, false, true], Some(2), false), Some(1));
        assert_eq!(step(&[false, true, false], None, false), Some(2));
        assert_eq!(step(&[true, true], None, true), None);
    }
}
