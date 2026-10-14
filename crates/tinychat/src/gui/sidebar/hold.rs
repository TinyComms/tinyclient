//! The Inbox's order is held while the user is aiming at it (`06-live-signals.md`, section 3):
//! a row moves to the top only when the pointer is not over the list and no arrow key moved
//! the cursor in the last two seconds. What arrives meanwhile changes its row in place, and the
//! reorder is applied once the list is let go.

use std::time::{Duration, Instant};

/// How long after an arrow-key move the order stays put.
pub const KEY_HOLD: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
pub enum Arrival {
    /// Nobody is aiming: read the list again in its new order.
    Reorder,
    /// Held. Change the row where it is; with a time, look again then (the key hold ends).
    InPlace(Option<Duration>),
}

#[derive(Default)]
pub struct Hold {
    hovered: bool,
    moved: Option<Instant>,
    /// A reorder arrived while held and has not been applied.
    pending: bool,
}

impl Hold {
    fn key_left(&self, now: Instant) -> Option<Duration> {
        let since = now.saturating_duration_since(self.moved?);
        (since < KEY_HOLD).then(|| KEY_HOLD - since)
    }

    /// Something in the list changed.
    pub fn arrived(&mut self, now: Instant) -> Arrival {
        let key = self.key_left(now);
        if !self.hovered && key.is_none() {
            self.pending = false;
            return Arrival::Reorder;
        }
        self.pending = true;
        // Under the pointer there is no time to wait for: leaving is the signal.
        Arrival::InPlace(if self.hovered { None } else { key })
    }

    pub fn key_moved(&mut self, now: Instant) {
        self.moved = Some(now);
    }

    /// The pointer came over the list or left it. True: apply the held reorder now.
    pub fn hover(&mut self, over: bool, now: Instant) -> bool {
        self.hovered = over;
        self.settle(now)
    }

    /// The key hold's timer fired. True: apply the held reorder now.
    pub fn settle(&mut self, now: Instant) -> bool {
        let free = !self.hovered && self.key_left(now).is_none();
        free && std::mem::take(&mut self.pending)
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
    fn the_order_holds_under_the_pointer_and_is_applied_on_leave() {
        let start = Instant::now();
        let mut hold = Hold::default();
        assert_eq!(hold.arrived(start), Arrival::Reorder);

        assert!(!hold.hover(true, start), "nothing was held back yet");
        assert_eq!(hold.arrived(start), Arrival::InPlace(None));
        assert_eq!(hold.arrived(start + Duration::from_secs(30)), Arrival::InPlace(None));
        assert!(hold.hover(false, start + Duration::from_secs(31)), "leaving applies the reorder once");
        assert!(!hold.hover(false, start + Duration::from_secs(32)));
        // Hovering with nothing new and leaving again reads nothing.
        assert!(!hold.hover(true, start + Duration::from_secs(33)));
        assert!(!hold.hover(false, start + Duration::from_secs(34)));
    }

    #[test]
    fn an_arrow_key_holds_the_order_for_two_seconds() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut hold = Hold::default();
        hold.key_moved(at(0));
        assert_eq!(hold.arrived(at(500)), Arrival::InPlace(Some(Duration::from_millis(1500))));
        assert!(!hold.settle(at(1900)), "still inside the hold");
        // Another move extends it.
        hold.key_moved(at(1900));
        assert!(!hold.settle(at(2100)));
        assert!(hold.settle(at(3900)));
        assert!(!hold.settle(at(4000)), "applied once");
        assert_eq!(hold.arrived(at(4000)), Arrival::Reorder);
    }

    #[test]
    fn leaving_during_a_key_hold_waits_for_the_keys_too() {
        let start = Instant::now();
        let mut hold = Hold::default();
        hold.hover(true, start);
        hold.key_moved(start);
        assert_eq!(hold.arrived(start), Arrival::InPlace(None));
        assert!(!hold.hover(false, start + Duration::from_secs(1)));
        assert!(hold.settle(start + KEY_HOLD));
    }
}
