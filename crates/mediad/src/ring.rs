//! A single-producer, single-consumer ring of samples, between a device's callback and the call's
//! tick. Neither side ever waits for the other, takes a lock or allocates: a device thread that
//! has to wait misses its deadline, and that is the glitch the user hears.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared {
    slots: Box<[UnsafeCell<f32>]>,
    mask: usize,
    /// Samples ever written and ever read. Each side owns one and only reads the other.
    written: AtomicUsize,
    read: AtomicUsize,
}

// SAFETY: the producer only writes slots the consumer has given back (`read..read + capacity`
// beyond `written`), and publishes them with a release store of `written`; the consumer only
// reads slots below `written` loaded with acquire, and gives them back the same way. No slot is
// ever touched by both at once.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn len(&self) -> usize {
        self.written.load(Ordering::Acquire).wrapping_sub(self.read.load(Ordering::Acquire))
    }
}

pub struct Producer(Arc<Shared>);
pub struct Consumer(Arc<Shared>);

/// A ring holding at least `capacity` samples.
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let size = capacity.max(2).next_power_of_two();
    let shared = Arc::new(Shared {
        slots: (0..size).map(|_| UnsafeCell::new(0.)).collect(),
        mask: size - 1,
        written: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
    });
    (Producer(shared.clone()), Consumer(shared))
}

impl Producer {
    /// Writes as much of `samples` as fits. Gives how many went in.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.0;
        let written = shared.written.load(Ordering::Relaxed);
        let free = shared.slots.len() - written.wrapping_sub(shared.read.load(Ordering::Acquire));
        let n = samples.len().min(free);
        for (i, &sample) in samples[..n].iter().enumerate() {
            // SAFETY: slot `written + i` is free (see `Shared`).
            unsafe { *shared.slots[(written + i) & shared.mask].get() = sample };
        }
        shared.written.store(written.wrapping_add(n), Ordering::Release);
        n
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.0.slots.len()
    }
}

impl Consumer {
    /// Reads up to `out.len()` samples. Gives how many came out.
    pub fn pop(&mut self, out: &mut [f32]) -> usize {
        let shared = &*self.0;
        let read = shared.read.load(Ordering::Relaxed);
        let held = shared.written.load(Ordering::Acquire).wrapping_sub(read);
        let n = out.len().min(held);
        for (i, sample) in out[..n].iter_mut().enumerate() {
            // SAFETY: slot `read + i` was published by the producer (see `Shared`).
            *sample = unsafe { *shared.slots[(read + i) & shared.mask].get() };
        }
        shared.read.store(read.wrapping_add(n), Ordering::Release);
        n
    }

    /// Drops up to `n` samples off the front. Gives how many went.
    pub fn skip(&mut self, n: usize) -> usize {
        let shared = &*self.0;
        let read = shared.read.load(Ordering::Relaxed);
        let n = n.min(shared.written.load(Ordering::Acquire).wrapping_sub(read));
        shared.read.store(read.wrapping_add(n), Ordering::Release);
        n
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_in_comes_out_in_order_across_the_wrap() {
        let (mut tx, mut rx) = ring(8);
        let mut out = [0f32; 8];
        for round in 0..10 {
            let base = round as f32 * 10.;
            assert_eq!(tx.push(&[base, base + 1., base + 2., base + 3., base + 4.]), 5);
            assert_eq!(rx.len(), 5);
            assert_eq!(rx.pop(&mut out[..3]), 3);
            assert_eq!(&out[..3], &[base, base + 1., base + 2.]);
            assert_eq!(rx.skip(5), 2);
        }
    }

    #[test]
    fn a_full_ring_takes_what_fits_and_an_empty_one_gives_nothing() {
        let (mut tx, mut rx) = ring(4);
        assert_eq!(tx.push(&[1., 2., 3., 4., 5., 6.]), 4);
        assert_eq!(tx.push(&[7.]), 0);
        let mut out = [0f32; 6];
        assert_eq!(rx.pop(&mut out), 4);
        assert_eq!(&out[..4], &[1., 2., 3., 4.]);
        assert_eq!(rx.pop(&mut out), 0);
    }

    #[test]
    fn two_threads_pass_a_count_without_losing_or_repeating_one() {
        let (mut tx, mut rx) = ring(64);
        let total = 200_000;
        let writer = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < total {
                let chunk: Vec<f32> = (next..(next + 7).min(total)).map(|n| n as f32).collect();
                next += tx.push(&chunk) as u32;
            }
        });
        let mut expected = 0u32;
        let mut out = [0f32; 13];
        while expected < total {
            let n = rx.pop(&mut out);
            for &sample in &out[..n] {
                assert_eq!(sample, expected as f32);
                expected += 1;
            }
        }
        writer.join().unwrap();
    }
}
