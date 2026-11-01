//! Readers and writers for the attachment tests: deterministic bulk data, short reads, and a sink
//! that only hashes.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// `len` bytes of a fixed pseudo-random stream (xorshift), never held in memory whole.
pub struct Pattern {
    state: u64,
    left: u64,
}

impl Pattern {
    pub fn new(seed: u64, len: u64) -> Pattern {
        Pattern {
            state: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            left: len,
        }
    }
}

impl Read for Pattern {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let take = buf.len().min(self.left as usize);
        for byte in &mut buf[..take] {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            *byte = (self.state >> 24) as u8;
        }
        self.left -= take as u64;
        Ok(take)
    }
}

pub fn pattern_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    Pattern::new(seed, len as u64)
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// A writer that keeps only a SHA-256 and a count.
#[derive(Default)]
pub struct HashSink {
    hasher: Sha256,
    pub written: u64,
}

impl HashSink {
    pub fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

impl Write for HashSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.written += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A reader that returns the data in reads of the given sizes, cycling through them (a size of 0
/// is read as 1; a short read is not the end).
pub struct Short<'a> {
    data: &'a [u8],
    at: usize,
    sizes: Vec<usize>,
    turn: usize,
}

impl<'a> Short<'a> {
    pub fn new(data: &'a [u8], sizes: Vec<usize>) -> Short<'a> {
        let sizes = if sizes.is_empty() { vec![1] } else { sizes };
        Short {
            data,
            at: 0,
            sizes,
            turn: 0,
        }
    }
}

impl Read for Short<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let size = self.sizes[self.turn % self.sizes.len()].max(1);
        self.turn += 1;
        let take = size.min(buf.len()).min(self.data.len() - self.at);
        buf[..take].copy_from_slice(&self.data[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}

/// A writer that accepts at most `size` bytes per call.
pub struct ShortWriter {
    pub out: Vec<u8>,
    pub size: usize,
}

impl Write for ShortWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let take = buf.len().min(self.size.max(1));
        self.out.extend_from_slice(&buf[..take]);
        Ok(take)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
