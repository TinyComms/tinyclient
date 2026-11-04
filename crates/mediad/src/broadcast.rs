//! The mailbox an iOS Broadcast Upload Extension writes and the app reads.
//!
//! A broadcast extension is another process, so a frame cannot be a function call. Both sides
//! map one file in the app group `group.dev.tinyclient.app`. The header is 64 bytes, then the
//! pixels. The writer bumps a sequence to odd, copies, then bumps it to even; a reader copies
//! only when the sequence is even at both ends. The same bytes are what `broadcast/SampleHandler.m`
//! writes, and the layout is checked on this machine without a phone.
//!
//! Offsets, little endian, which is the endianness of every iPhone and of the test here: magic
//! `u32` 0, version `u32` 4, state `u32` 8, seq `u32` 12, width `u32` 16, height `u32` 20,
//! format `u32` 24, data_len `u32` 28, timestamp `u64` 32, reserved through 64.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// App group the app and the extension both entitle.
pub const GROUP: &str = "group.dev.tinyclient.app";
/// File name inside that group's container.
pub const FILE_NAME: &str = "tinyclient-broadcast.bin";
/// `TNYB`.
pub const MAGIC: u32 = 0x544E_5942;
pub const VERSION: u32 = 1;
pub const HEADER: usize = 64;
/// Long side the extension scales to. Even.
pub const MAX_SIDE: u32 = 1280;
pub const MAX_BYTES: usize = 1280 * 1280 * 4;
pub const FILE_LEN: usize = HEADER + MAX_BYTES;

pub const STATE_IDLE: u32 = 0;
pub const STATE_LISTENING: u32 = 1;
pub const STATE_BROADCASTING: u32 = 2;
pub const STATE_STOP: u32 = 3;

pub const FORMAT_BGRA: u32 = 1;
pub const FORMAT_NV12: u32 = 2;

#[repr(C)]
struct Header {
    magic: u32,
    version: u32,
    state: AtomicU32,
    seq: AtomicU32,
    width: u32,
    height: u32,
    format: u32,
    data_len: u32,
    timestamp_us: AtomicU64,
    reserved: [u32; 6],
}

const _: () = assert!(size_of::<Header>() == HEADER);
const _: () = assert!(align_of::<Header>() == 8);

/// One frame copied out of the mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailFrame {
    pub seq: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

/// A shared mapping of the mailbox file. The other process sees the same bytes.
pub struct Map {
    ptr: *mut u8,
    len: usize,
    /// `map_file` owns an `mmap`. A test buffer does not, and must not be unmapped.
    mapped: bool,
}

unsafe impl Send for Map {}
unsafe impl Sync for Map {}

impl Drop for Map {
    fn drop(&mut self) {
        if !self.mapped {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

impl Map {
    fn header(&self) -> &Header {
        // SAFETY: `map_file` and the tests align the allocation to 8 and size it past the header.
        // The atomics are the same layout C11 `_Atomic` has on this machine.
        unsafe { &*self.ptr.cast::<Header>() }
    }

    fn pixels(&self) -> *mut u8 {
        // SAFETY: the mapping is `FILE_LEN` bytes and the payload starts at `HEADER`.
        unsafe { self.ptr.add(HEADER) }
    }
}

/// Maps `path`, creating it at [`FILE_LEN`] bytes when it is short. Shared, so the extension's
/// writes show up here.
#[cfg(unix)]
pub fn map_file(path: &std::path::Path) -> std::io::Result<Map> {
    use std::os::fd::IntoRawFd;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.set_len(FILE_LEN as u64)?;
    let fd = file.into_raw_fd();
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            FILE_LEN,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    unsafe { libc::close(fd) };
    if ptr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Map {
        ptr: ptr.cast(),
        len: FILE_LEN,
        mapped: true,
    })
}

/// Writes the magic and marks the app as waiting. An old frame is hidden by a zero length, not
/// by wiping the payload.
pub fn prepare(map: &Map) {
    let header = map.header();
    // SAFETY: the header fields sit at the documented offsets and the mapping covers them.
    unsafe {
        std::ptr::write_volatile(std::ptr::addr_of!(header.magic).cast_mut(), MAGIC);
        std::ptr::write_volatile(std::ptr::addr_of!(header.version).cast_mut(), VERSION);
        std::ptr::write_volatile(std::ptr::addr_of!(header.width).cast_mut(), 0);
        std::ptr::write_volatile(std::ptr::addr_of!(header.height).cast_mut(), 0);
        std::ptr::write_volatile(std::ptr::addr_of!(header.format).cast_mut(), 0);
        std::ptr::write_volatile(std::ptr::addr_of!(header.data_len).cast_mut(), 0);
    }
    header.timestamp_us.store(0, Ordering::Relaxed);
    header.seq.store(0, Ordering::Release);
    header.state.store(STATE_LISTENING, Ordering::Release);
}

/// The state byte the extension watches.
pub fn set_state(map: &Map, state: u32) {
    map.header().state.store(state, Ordering::Release);
}

pub fn state(map: &Map) -> u32 {
    map.header().state.load(Ordering::Acquire)
}

/// Publishes one frame. False when the mailbox was not prepared or the pixels do not fit.
pub fn publish(
    map: &Map,
    width: u32,
    height: u32,
    format: u32,
    pixels: &[u8],
    timestamp_us: u64,
) -> bool {
    let header = map.header();
    // SAFETY: magic is written by `prepare` and read once, before the sequence moves.
    let magic = unsafe { std::ptr::read_volatile(std::ptr::addr_of!(header.magic)) };
    if magic != MAGIC || pixels.len() > MAX_BYTES || map.len < HEADER + pixels.len() {
        return false;
    }
    // The acquire half prevents payload writes moving before the odd sequence is visible.
    let seq = header.seq.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the payload mapping covers `pixels`, and a reader ignores the bytes while the
    // sequence is odd.
    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), map.pixels(), pixels.len());
        std::ptr::write_volatile(std::ptr::addr_of!(header.width).cast_mut(), width);
        std::ptr::write_volatile(std::ptr::addr_of!(header.height).cast_mut(), height);
        std::ptr::write_volatile(std::ptr::addr_of!(header.format).cast_mut(), format);
        std::ptr::write_volatile(
            std::ptr::addr_of!(header.data_len).cast_mut(),
            pixels.len() as u32,
        );
    }
    header.timestamp_us.store(timestamp_us, Ordering::Relaxed);
    header.seq.store(seq.wrapping_add(2), Ordering::Release);
    true
}

/// The latest stable frame, or nothing while a write is in progress or the share has stopped.
pub fn read(map: &Map) -> Option<MailFrame> {
    let header = map.header();
    let state = header.state.load(Ordering::Acquire);
    if state != STATE_LISTENING && state != STATE_BROADCASTING {
        return None;
    }
    let seq1 = header.seq.load(Ordering::Acquire);
    if seq1 & 1 == 1 {
        return None;
    }
    // SAFETY: the sequence is even, so the writer is not in the copy. The second load below
    // drops the copy if a new write started.
    let (width, height, format, data_len, timestamp_us, data) = unsafe {
        let width = std::ptr::read_volatile(std::ptr::addr_of!(header.width));
        let height = std::ptr::read_volatile(std::ptr::addr_of!(header.height));
        let format = std::ptr::read_volatile(std::ptr::addr_of!(header.format));
        let data_len = std::ptr::read_volatile(std::ptr::addr_of!(header.data_len)) as usize;
        let timestamp_us = header.timestamp_us.load(Ordering::Relaxed);
        if data_len == 0 || data_len > MAX_BYTES || HEADER + data_len > map.len {
            return None;
        }
        let mut data = vec![0u8; data_len];
        std::ptr::copy_nonoverlapping(map.pixels(), data.as_mut_ptr(), data_len);
        (width, height, format, data_len, timestamp_us, data)
    };
    std::sync::atomic::fence(Ordering::Acquire);
    if header.seq.load(Ordering::Acquire) != seq1 {
        return None;
    }
    let _ = data_len;
    Some(MailFrame {
        seq: seq1,
        width,
        height,
        format,
        data,
        timestamp_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    struct Buf {
        ptr: *mut u8,
        layout: Layout,
    }

    impl Buf {
        fn new() -> Buf {
            let layout = Layout::from_size_align(FILE_LEN, 8).unwrap();
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null());
            Buf { ptr, layout }
        }

        fn map(&self) -> Map {
            Map {
                ptr: self.ptr,
                len: FILE_LEN,
                mapped: false,
            }
        }
    }

    impl Drop for Buf {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) };
        }
    }

    // Windows cannot run ReplayKit. Check the actual extension's publication boundaries here,
    // alongside the reader's behavior while a producer is paused in its pixel copy below.
    #[test]
    fn extension_hides_pixel_and_metadata_writes_until_publication() {
        let source = include_str!("../../../mobile/ios/Broadcast/SampleHandler.m");
        let producer = source.split("- (void)publish:").nth(1).unwrap()
            .split("- (void)fitWidth:").next().unwrap();
        let busy = producer.find("atomic_fetch_add_explicit(&_header->seq").or_else(||
            producer.find("atomic_store_explicit(&_header->seq, seq + 1u")).expect("marks publication busy");
        let done = producer.find("atomic_store_explicit(&_header->seq, seq + 2u").expect("publishes stable frame");
        assert!(producer[busy..].split(';').next().unwrap().contains("memory_order_acq_rel"), "the busy mark must also keep later payload writes after it");
        assert!(producer[done..].split(';').next().unwrap().contains("memory_order_release"), "publish all writes before marking the frame stable");
        for payload in ["uint8_t *dst = _pixels", "uint8_t *dstY = _pixels", "_header->width =", "_header->height =", "_header->format =", "_header->data_len =", "atomic_store_explicit(&_header->timestamp_us"] {
            let write = producer.find(payload).expect(payload);
            assert!(busy < write && write < done, "{payload} must be inside the odd sequence interval");
        }
    }

    #[test]
    fn a_reader_rejects_a_producer_paused_mid_copy() {
        let buf = Buf::new();
        let map = buf.map();
        prepare(&map);
        assert!(publish(&map, 2, 2, FORMAT_BGRA, &[1; 16], 10));
        let seq = map.header().seq.fetch_add(1, Ordering::AcqRel);
        unsafe { std::ptr::write_bytes(map.pixels(), 2, 8) };
        assert!(read(&map).is_none(), "a half-copied frame must not escape");
        unsafe { std::ptr::write_bytes(map.pixels().add(8), 2, 8) };
        map.header().timestamp_us.store(20, Ordering::Relaxed);
        map.header().seq.store(seq + 2, Ordering::Release);
        let frame = read(&map).unwrap();
        assert_eq!(frame.data, [2; 16]);
        assert_eq!(frame.timestamp_us, 20);
    }

    #[test]
    fn a_published_frame_reads_back_and_an_odd_sequence_does_not() {
        let buf = Buf::new();
        let map = buf.map();
        assert!(read(&map).is_none());
        prepare(&map);
        assert_eq!(state(&map), STATE_LISTENING);
        let pixels = vec![7u8, 8, 9, 10];
        assert!(publish(&map, 1, 1, FORMAT_BGRA, &pixels, 42));
        let frame = read(&map).expect("frame");
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.format, FORMAT_BGRA);
        assert_eq!(frame.data, pixels);
        assert_eq!(frame.timestamp_us, 42);
        assert_eq!(frame.seq % 2, 0);

        map.header().seq.store(1, Ordering::Release);
        assert!(read(&map).is_none());

        set_state(&map, STATE_STOP);
        map.header().seq.store(2, Ordering::Release);
        assert!(read(&map).is_none());
    }
}
