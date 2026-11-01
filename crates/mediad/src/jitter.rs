//! One sender's packets, put back in order and handed out one every 20 ms.

use std::collections::BTreeMap;

/// Packets held before playing starts: 60 ms rides out ordinary reordering.
const START: usize = 3;
/// More than a second queued means the clocks disagree; the oldest go.
const CAPACITY: usize = 50;
/// Ticks with nothing queued before the talk spurt is taken to be over: 400 ms. A shorter
/// give-up (the old 100 ms) cuts a sentence off while a packet is still on a long link, and
/// the next words then wait to fill the buffer again.
const IDLE: u32 = 20;
/// Ticks a hole of two packets or more is waited out, once, while a later packet is already
/// here: 40 ms. That is enough for one to arrive out of order. The wait is not a loss: concealing
/// it makes the decoder drop its state, and the frames after a real loss then come out too loud.
/// After the wait the whole gap is skipped, so the packet that follows can carry a copy of the
/// last lost frame. With the very next packet already here there is no wait: each one spent
/// holding is 20 ms the buffer never gives back.
const REORDER: u32 = 2;
/// A jump this large in sequence numbers is a new stream on the same SSRC.
const RESTART: i64 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    Packet(Vec<u8>),
    /// The first packet of a talk spurt, after something other than a clean run of sequence
    /// numbers. Time passed that the decoder did not see.
    Spurt(Vec<u8>),
    /// A later packet is already here and the missing one may still arrive. Keep the voice
    /// going without touching the decoder, so a copy in the next packet can still rebuild a
    /// real loss.
    Hold,
    /// The packets due now never came, and a later one did: the decoder conceals this many.
    Lost(u32),
    /// Nothing is queued while a packet is due. It may be running late, or the sender may have
    /// stopped: only the next packet can say, and nothing counts as lost until then.
    Late,
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
    /// Ticks spent waiting on the current hole, while a later packet is already queued.
    grace: u32,
    /// The next packet played starts a spurt, not the one the last was followed by.
    spurt: bool,
    pub late: u64,
    pub lost: u64,
    /// Packets taken in time to be played.
    pub received: u64,
    /// The frame played last was quiet, as the decoder found it.
    pub hushed: bool,
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
            *self = Jitter { late: self.late, lost: self.lost, received: self.received, ..Jitter::default() };
            unwrapped = i64::from(sequence);
        }
        if self.due.is_some_and(|due| unwrapped < due) {
            self.late += 1;
            return;
        }
        self.newest = Some(self.newest.map_or(unwrapped, |newest| newest.max(unwrapped)));
        self.received += 1;
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
                return match self.pop() {
                    Pop::Packet(payload) if std::mem::take(&mut self.spurt) => Pop::Spurt(payload),
                    pop => pop,
                };
            }
            return Pop::Empty;
        };
        if let Some(payload) = self.packets.remove(&due) {
            self.due = Some(due + 1);
            self.idle = 0;
            self.grace = 0;
            return Pop::Packet(payload);
        }
        if let Some(&next) = self.packets.keys().next() {
            let dry = self.idle > 0;
            if self.hushed {
                // The frame before the hole was quiet: the sender's gate shut after its trail
                // of silence, and this is the next thing said. The RTP clock ran on all the
                // while, so the sequence jumps, but nothing was lost. Walking that hole frame by
                // frame is what made words arrive late after a pause.
                self.restart();
                return self.pop();
            }
            // A real hole. Wait out a reorder if one could still fill it, once for the whole gap.
            // A dry queue has waited already.
            let gap = next - due;
            if gap > 1 && !dry && self.grace < REORDER {
                self.grace += 1;
                return Pop::Hold;
            }
            self.grace = 0;
            self.idle = 0;
            self.due = Some(next);
            self.lost += gap as u64;
            return Pop::Lost(gap as u32);
        }
        self.grace = 0;
        self.idle += 1;
        if self.idle >= IDLE {
            self.restart();
            return Pop::Empty;
        }
        Pop::Late
    }

    /// The spurt ended. The next one starts by filling again.
    fn restart(&mut self) {
        self.due = None;
        self.idle = 0;
        self.grace = 0;
        self.spurt = true;
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
        self.grace = 0;
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
        assert_eq!(jitter.pop(), Pop::Lost(1), "4 is here, so no tick is spent on 3");
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
    fn a_gap_of_three_hundred_milliseconds_does_not_end_the_spurt() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
        }
        // Long enough to outlast str0m's old 15-packet hold, and the old 100 ms give-up.
        for _ in 0..15 {
            assert_eq!(jitter.pop(), Pop::Late);
        }
        fill(&mut jitter, &[4]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(4)));
    }

    #[test]
    fn a_reordered_packet_is_played_when_it_arrives_within_two_ticks() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 5]);
        assert_eq!(jitter.pop(), Pop::Packet(packet(1)));
        assert_eq!(jitter.pop(), Pop::Packet(packet(2)));
        assert_eq!(jitter.pop(), Pop::Hold, "3 is not here yet");
        fill(&mut jitter, &[3]);
        assert_eq!(jitter.late, 0);
        assert_eq!(jitter.pop(), Pop::Packet(packet(3)));
    }

    #[test]
    fn a_hole_costs_one_grace_period_and_not_one_for_each_frame() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3, 8, 9]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
        }
        assert_eq!(jitter.pop(), Pop::Hold);
        assert_eq!(jitter.pop(), Pop::Hold);
        assert_eq!(jitter.pop(), Pop::Lost(4), "the whole gap at once");
        assert_eq!(jitter.next(), Some(&packet(8)[..]), "its last frame may be in here");
        assert_eq!(jitter.pop(), Pop::Packet(packet(8)));
        assert_eq!(jitter.lost, 4);
    }

    #[test]
    fn a_hole_after_a_dry_queue_in_mid_speech_is_lost_without_more_waiting() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
        }
        assert_eq!(jitter.pop(), Pop::Late);
        assert_eq!(jitter.pop(), Pop::Late);
        fill(&mut jitter, &[7, 8]);
        assert_eq!(jitter.pop(), Pop::Lost(3));
        assert_eq!(jitter.pop(), Pop::Packet(packet(7)));
    }

    /// The bug: with the gate shut the sequence still runs on (it comes from RTP time), so a pause
    /// of half a second was a hole of 25 frames, walked at three ticks each: words came up to
    /// 700 ms late, and every frame of the pause counted as lost.
    #[test]
    fn a_pause_after_a_quiet_frame_is_a_new_spurt_and_loses_nothing() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
            jitter.hushed = true;
        }
        for _ in 0..4 {
            assert_eq!(jitter.pop(), Pop::Late);
        }
        fill(&mut jitter, &[28]);
        assert_eq!(jitter.pop(), Pop::Empty, "refilling, not walking the hole");
        fill(&mut jitter, &[29, 30]);
        assert_eq!(jitter.pop(), Pop::Spurt(packet(28)));
        assert_eq!(jitter.pop(), Pop::Packet(packet(29)));
        assert_eq!(jitter.lost, 0);
    }

    /// With a deep buffer the words after a short pause get here before the frames before it have
    /// played, so the queue never runs dry. It is still a pause.
    #[test]
    fn a_pause_that_never_empties_the_queue_is_a_pause_too() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3, 4, 20, 21, 22]);
        for _ in 0..4 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
            jitter.hushed = true;
        }
        assert_eq!(jitter.pop(), Pop::Spurt(packet(20)));
        assert_eq!(jitter.lost, 0);
    }

    #[test]
    fn a_spurt_ends_after_a_few_concealed_frames_and_the_next_one_refills() {
        let mut jitter = Jitter::default();
        fill(&mut jitter, &[1, 2, 3]);
        for _ in 0..3 {
            assert!(matches!(jitter.pop(), Pop::Packet(_)));
        }
        let after: Vec<_> = (0..IDLE).map(|_| jitter.pop()).collect();
        assert!(after[..IDLE as usize - 1].iter().all(|pop| *pop == Pop::Late));
        assert_eq!(after[IDLE as usize - 1], Pop::Empty);
        assert_eq!(jitter.lost, 0, "a sentence that ended lost nothing");
        // The sender resumes much later, sequence numbers continuing.
        fill(&mut jitter, &[4]);
        assert_eq!(jitter.pop(), Pop::Empty, "filling again");
        fill(&mut jitter, &[5, 6]);
        assert_eq!(jitter.pop(), Pop::Spurt(packet(4)));
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
