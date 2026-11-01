//! Memory stays bounded while a large attachment streams through, whatever its size. This file
//! holds one test on purpose: it installs a counting allocator, and a second test running beside it
//! would count too.

mod streams;

use e2ee::attachment::{self, AttachmentKey, Expect};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use streams::*;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            if new_size >= layout.size() {
                let live = LIVE.fetch_add(new_size - layout.size(), Ordering::SeqCst)
                    + (new_size - layout.size());
                PEAK.fetch_max(live, Ordering::SeqCst);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
            }
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `work` and returns the most bytes it held on the heap above where it started.
fn peak_during<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let result = work();
    (result, PEAK.load(Ordering::SeqCst).saturating_sub(base))
}

/// A sink that takes the sealed blob without keeping it, and a source that makes it again: the
/// whole path holds no more than the library's own buffers.
struct Tee {
    sink: HashSink,
}

#[test]
fn sealing_and_opening_hold_a_bounded_amount_whatever_the_size() {
    let key = AttachmentKey::from_bytes([9; 32]);
    let mut results = Vec::new();
    let header = attachment::Header {
        chunk_log2: 16,
        nonce_prefix: [3; 7],
    };
    for len in [1u64 << 20, 8 << 20, 64 << 20] {
        // Seal: pattern in, hash out.
        let (info, seal_peak) = peak_during(|| {
            let mut tee = Tee {
                sink: HashSink::default(),
            };
            let info = attachment::seal_with_header(
                &key,
                b"mem",
                header,
                Pattern::new(5, len),
                &mut tee.sink,
            )
            .unwrap();
            assert_eq!(tee.sink.written, info.sealed_len);
            info
        });
        // Open needs the sealed blob to read from: made outside the measurement, the same blob.
        let mut sealed = Vec::with_capacity(info.sealed_len as usize);
        attachment::seal_with_header(&key, b"mem", header, Pattern::new(5, len), &mut sealed)
            .unwrap();
        assert_eq!(sealed.len() as u64, info.sealed_len);
        let (_, open_peak) = peak_during(|| {
            let mut sink = HashSink::default();
            let opened = attachment::open(
                &key,
                b"mem",
                sealed.as_slice(),
                &mut sink,
                Expect::default(),
            )
            .unwrap();
            assert_eq!(opened.plain_len, len);
            assert_eq!(sink.written, len);
        });
        eprintln!(
            "{} MiB: seal peak {} KiB, open peak {} KiB",
            len >> 20,
            seal_peak / 1024,
            open_peak / 1024
        );
        results.push((len, seal_peak, open_peak));
    }
    for (len, seal_peak, open_peak) in &results {
        // Two 64 KiB chunks and a little more. Generous: 1 MiB, for a 64 MiB file.
        assert!(
            *seal_peak < 1 << 20,
            "sealing {len} bytes held {seal_peak} bytes"
        );
        assert!(
            *open_peak < 1 << 20,
            "opening {len} bytes held {open_peak} bytes"
        );
    }
    // And it does not grow with the file: the biggest holds about what the smallest did.
    let (small, big) = (results[0].1, results[2].1);
    assert!(
        big < small + 64 * 1024,
        "seal memory grew from {small} to {big}"
    );
    let (small, big) = (results[0].2, results[2].2);
    assert!(
        big < small + 64 * 1024,
        "open memory grew from {small} to {big}"
    );
}
