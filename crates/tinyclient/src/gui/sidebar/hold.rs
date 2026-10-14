//! The Inbox's order is held while the user is aiming at it (`design/polish/04-unified.md`
//! §2): while the pointer is over the list, or a list key moved in the last two seconds, no row
//! moves. What arrives meanwhile changes its row in place and waits in the "n new" pill; the
//! new order is applied on the pill, or a second after the list is let go.

use std::time::{Duration, Instant};

/// How long after a list key the order stays put.
pub const KEY_HOLD: Duration = Duration::from_secs(2);
/// How long after the pointer leaves before the waiting order is applied.
pub const LEAVE_HOLD: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq)]
pub enum Arrival {
    /// Read the list again; the reader holds its order when it must.
    Reorder,
    /// Change the row where it is. Never answered now: the whole list is read either way, so
    /// that rows new to it can be counted in the pill.
    #[allow(dead_code)]
    InPlace(Option<Duration>),
}

#[derive(Default)]
pub struct Hold {
    hovered: bool,
    left: Option<Instant>,
    moved: Option<Instant>,
    /// A new order arrived while held and has not been applied.
    pending: bool,
}

impl Hold {
    fn wait(since: Option<Instant>, hold: Duration, now: Instant) -> Option<Duration> {
        let since = now.saturating_duration_since(since?);
        (since < hold).then(|| hold - since)
    }

    /// How long until nothing holds the order; `None` while the pointer is over the list.
    pub fn free_in(&self, now: Instant) -> Option<Duration> {
        if self.hovered {
            return None;
        }
        let key = Self::wait(self.moved, KEY_HOLD, now).unwrap_or_default();
        let left = Self::wait(self.left, LEAVE_HOLD, now).unwrap_or_default();
        Some(key.max(left))
    }

    pub fn holding(&self, now: Instant) -> bool {
        self.free_in(now).is_none_or(|wait| !wait.is_zero())
    }

    /// Something in the list changed.
    pub fn arrived(&mut self, _now: Instant) -> Arrival {
        Arrival::Reorder
    }

    /// A new order came in and was held back.
    pub fn held_back(&mut self) {
        self.pending = true;
    }

    pub fn key_moved(&mut self, now: Instant) {
        self.moved = Some(now);
    }

    /// The pointer came over the list or left it. Leaving starts the second's wait.
    pub fn hover(&mut self, over: bool, now: Instant) -> bool {
        if self.hovered && !over {
            self.left = Some(now);
        }
        self.hovered = over;
        self.settle(now)
    }

    /// A timer fired. True: apply the held order now.
    pub fn settle(&mut self, now: Instant) -> bool {
        !self.holding(now) && std::mem::take(&mut self.pending)
    }

    /// The pill was clicked, or the list was left for another: whatever waited is applied.
    pub fn release(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }

    /// Another list is on show: nothing is held any more.
    pub fn reset(&mut self) {
        *self = Hold::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_holds_under_the_pointer_and_is_applied_a_second_after_leaving() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut hold = Hold::default();
        assert!(!hold.holding(start));
        hold.hover(true, at(0));
        assert!(hold.holding(at(10)));
        hold.held_back();
        assert!(!hold.hover(false, at(5_000)), "leaving waits a second");
        assert_eq!(hold.free_in(at(5_400)), Some(Duration::from_millis(600)));
        assert!(!hold.settle(at(5_900)));
        assert!(hold.settle(at(6_000)), "then applies once");
        assert!(!hold.settle(at(6_100)));
    }

    #[test]
    fn a_list_key_holds_the_order_for_two_seconds_and_the_pill_applies_at_once() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut hold = Hold::default();
        hold.key_moved(at(0));
        hold.held_back();
        assert_eq!(hold.free_in(at(500)), Some(Duration::from_millis(1500)));
        hold.key_moved(at(1900));
        assert!(!hold.settle(at(2100)));
        assert!(hold.settle(at(3900)));
        hold.held_back();
        assert!(hold.release() && !hold.release());
        assert_eq!(hold.arrived(at(0)), Arrival::Reorder);
    }
}
