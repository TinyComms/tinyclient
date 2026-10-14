//! One sender's packets, put back in order and handed out one every 20 ms.

use std::collections::BTreeMap;

/// Packets held before playing starts: 60 ms rides out ordinary reordering.
const START: usize = 3;
/// More than a second queued means the clocks disagree; the oldest go.
const CAPACITY: usize = 50;
/// Ticks with nothing to play before the talk spurt is taken to be over.
const IDLE: u32 = 5;
/// A jump this large in sequence numbers is a new stream on the same SSRC.
const RESTART: i64 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    Packet(Vec<u8>),
    /// The packet due now never came: the decoder conceals it.
    Lost,
    /// Nothing is due: the sender is not talking, or we are still filling.
    Empty,
}

#[derive(Default)]
pub struct Jitter {
    /// By sequence number, unwrapped so 65535 is followed by 65536.
    packets: BTreeMap<i64, Vec<u8>>,
    /// The newest sequence number seen, unwrapped.
    newest: Option<i64>,
    /// The sequence number due next, once playing.
    due: Option<i64>,
    idle: u32,
    pub late: u64,
    pub lost: u64,
}

impl Jitter {
    fn unwrap(&self, sequence: u16) -> i64 {
        let Some(newest) = self.newest else {
            return i64::from(sequence);
        };
        // The 16-bit distance from the newest, taken as signed: a little behind or ahead.
        let delta = sequence.wrapping_sub(newest as u16) as i16;
        newest + i64::from(delta)
    }

    pub fn insert(&mut self, sequence: u16, payload: Vec<u8>) {
        let mut unwrapped = self.unwrap(sequence);
        if self.newest.is_some_and(|newest| (unwrapped - newest).abs() > RESTART) {
            *self = Jitter { late: self.late, lost: self.lost, ..Jitter::default() };
            unwrapped = i64::from(sequence);
        }
        if self.due.is_some_and(|due| unwrapped < due) {
            self.late += 1;
            return;
        }
        self.newest = Some(self.newest.map_or(unwrapped, |newest| newest.max(unwrapped)));
        self.packets.insert(unwrapped, payload);
        while self.packets.len() > CAPACITY {
            let (oldest, _) = self.packets.pop_first().unwrap();
            if self.due.is_some() {
                self.due = Some(oldest + 1);
            }
        }
    }

    /// Called once every 20 ms.
    pub fn pop(&mut self) -> Pop {
        let Some(due) = self.due else {
            // Filling. Also start when a short burst stopped arriving, so it is not held forever.
            let waiting = !self.packets.is_empty();
            self.idle = if waiting { self.idle + 1 } else { 0 };
            if self.packets.len() >= START || (waiting && self.idle >= START as u32) {
                self.idle = 0;
                self.due = self.packets.keys().next().copied();
                return self.pop();
            }
            return Pop::Empty;
        };
        if let Some(payload) = self.packets.remove(&due) {
            self.due = Some(due + 1);
            self.idle = 0;
            return Pop::Packet(payload);
        }
        if !self.packets.is_empty() {
            // A hole with later packets behind it.
            self.due = Some(due + 1);
            self.lost += 1;
            return Pop::Lost;
        }
        self.idle += 1;
        if self.idle >= IDLE {
            // The spurt ended. The next one starts by filling again.
            self.due = None;
            self.idle = 0;
            return Pop::Empty;
        }
        // Late or lost: conceal for a few frames rather than cut to silence.
        Pop::Lost
    }

    /// The packet due next, if it is here. After a [`Pop::Lost`] this is the one that may carry
    /// a copy of the frame that was lost.
    pub fn next(&self) -> Option<&[u8]> {
        self.packets.get(&self.due?).map(Vec::as_slice)
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.due = None;
        self.idle = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(n: u16) -> Vec<u8> {
        n.to_be_bytes().to_vec()
    }

    fn fill(jitter: &mut Jitter, sequences: &[u16]) {
        for &n in sequences {
            jitter.insert(n, packet(n));
        }
    }

    #[test]
    fn nothing_plays_until_three_packets_are_held() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[10, 11]);
        assert_eq!(jitter.pop(), Pop::Empty);
        fill(&mut jitter, &[12]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(10)));
    }

    #[test]
    fn reordered_packets_come_out_in_order() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[7, 9, 8, 10]);
        let played: Vec<_> = (0..4).map(|_| jitter.pop()).collect();
        assert_eq!(played, [7, 8, 9, 10].map(|n| Pop::Packet(packet(n))));
    }

    #[test]
    fn a_hole_is_concealed_and_its_late_packet_dropped() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 4, 5]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(1)));
        assert_eq!(jitter.next(), Some(&packet(2)[..]));
        assert_eq!(jitter.pop(), Pop::Packet(packet(2)));
        assert_eq!(jitter.next(), None, "3 is missing");
        assert_eq!(jitter.pop(), Pop::Lost);
        assert_eq!(jitter.next(), Some(&packet(4)[..]), "the packet after the hole, for its copy of 3");
        fill(&mut jitter, &[3]);
        assert_eq!(jitter.late, 1);
        assert_eq!(jitter.pop(), Pop::Packet(packet(4)));
        assert_eq!(jitter.lost, 1);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[65_534, 65_535, 0, 1]);
        let played: Vec<_> = (0..4).map(|_| jitter.pop()).collect();
        assert_eq!(played, [65_534, 65_535, 0, 1].map(|n| Pop::Packet(packet(n))));
    }

    #[test]
    fn a_spurt_ends_after_a_few_concealed_frames_and_the_next_one_refills() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
        }
        let after: Vec<_> = (0..IDLE).map(|_| jitter.pop()).collect();
        assert_eq!(after[..IDLE as usize - 1], [Pop::Lost, Pop::Lost, Pop::Lost, Pop::Lost]);
        assert_eq!(after[IDLE as usize - 1], Pop::Empty);
        // The sender resumes much later, sequence numbers continuing.
        fill(&mut jitter, &[4]);
        assert_eq!(jitter.pop(), Pop::Empty, "filling again");
        fill(&mut jitter, &[5, 6]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(4)));
    }

    #[test]
    fn a_short_burst_is_not_held_forever() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1]);
        let played: Vec<_> = (0..3).map(|_| jitter.pop()).collect();
        assert_eq!(played, [Pop::Empty, Pop::Empty, Pop::Packet(packet(1))]);
    }

    #[test]
    fn a_stream_that_restarts_far_away_is_followed() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[100, 101, 102]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(100)));
        fill(&mut jitter, &[30_000, 30_001, 30_002]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(30_000)));
    }

    #[test]
    fn a_flood_keeps_only_the_newest_second() {
        let mut jitter = Jitter::default();
        for n in 0..200 {
            jitter.insert(n, packet(n));
        }
        assert_eq!(jitter.pop(), Pop::Packet(packet(150)));
    }
}
