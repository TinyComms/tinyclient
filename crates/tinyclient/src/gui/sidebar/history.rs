//! Back and forward: the places the user has been, replayed. A place is whatever the caller
//! says it is; nothing here knows about conversations.

/// Places remembered in each direction.
pub const HISTORY: usize = 50;

pub struct History<T> {
    back: Vec<T>,
    forward: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        History { back: Vec::new(), forward: Vec::new() }
    }
}

impl<T: PartialEq> History<T> {
    /// The user is leaving `here` for somewhere new: forward no longer leads anywhere.
    pub fn leave(&mut self, here: T) {
        if self.back.last() != Some(&here) {
            self.back.push(here);
            if self.back.len() > HISTORY {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// One step back (or forward) from `here`: where to go, if anywhere.
    pub fn travel(&mut self, back: bool, here: T) -> Option<T> {
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        let there = from.pop()?;
        to.push(here);
        Some(there)
    }

    pub fn can(&self, back: bool) -> bool {
        !(if back { &self.back } else { &self.forward }).is_empty()
    }

    /// The places behind, newest first.
    pub fn behind(&self) -> impl Iterator<Item = &T> {
        self.back.iter().rev()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_then_forward_returns_and_a_new_place_ends_forward() {
        let mut history = History::default();
        assert!(!history.can(true) && !history.can(false));
        history.leave("a");
        history.leave("b");
        // Now at c.
        assert_eq!(history.travel(true, "c"), Some("b"));
        assert_eq!(history.travel(true, "b"), Some("a"));
        assert_eq!(history.travel(true, "a"), None);
        assert!(history.can(false));
        assert_eq!(history.travel(false, "a"), Some("b"));
        assert_eq!(history.travel(false, "b"), Some("c"));
        assert_eq!(history.travel(false, "c"), None);
        // Back to b, then somewhere new: c is no longer ahead.
        assert_eq!(history.travel(true, "c"), Some("b"));
        history.leave("b");
        assert!(!history.can(false));
        assert_eq!(history.behind().copied().collect::<Vec<_>>(), ["b", "a"]);
    }

    #[test]
    fn leaving_the_same_place_twice_is_one_step_and_the_oldest_falls_off() {
        let mut history = History::default();
        history.leave(1);
        history.leave(1);
        assert_eq!(history.behind().count(), 1);
        for place in 2..=HISTORY + 10 {
            history.leave(place);
        }
        assert_eq!(history.behind().count(), HISTORY);
        assert_eq!(history.behind().last(), Some(&11));
    }
}
