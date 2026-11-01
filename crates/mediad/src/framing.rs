//! ICE-TCP framing for a Tiny server call: RFC 4571, as RFC 6544 uses it. Each packet is a
//! 2-byte big-endian length, then that many bytes. The same rules as the server's
//! (`crates/sfu/src/tcp.rs` in tinyserver): frames of 1 to 2,000 bytes, and a bounded backlog.

use mio::net::TcpStream;
use std::io::{self, ErrorKind, Read, Write};

/// The largest frame either side sends or takes. Larger than any packet str0m makes.
pub const FRAME_MAX: usize = 2000;
/// Bytes waiting to go out on one stream. A packet that would pass it is dropped, as UDP
/// would lose it. Audio shares this queue, so what waits here is what every voice frame waits
/// behind: at 2 Mb/s this is 160 ms, which is as long as a frame is still worth sending.
const BACKLOG: usize = 32 * 1024;
/// One read from a stream.
const CHUNK: usize = 4096;

/// A frame length of zero or over the cap. The stream is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadFrame;

/// Splits a byte stream into frames.
#[derive(Debug, Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame, if one is buffered.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, BadFrame> {
        let Some(&[high, low]) = self.buf.get(..2) else {
            return Ok(None);
        };
        let len = usize::from(u16::from_be_bytes([high, low]));
        if len == 0 || len > FRAME_MAX {
            return Err(BadFrame);
        }
        let Some(frame) = self.buf.get(2..2 + len) else {
            return Ok(None);
        };
        let frame = frame.to_vec();
        self.buf.drain(..2 + len);
        Ok(Some(frame))
    }
}

/// Appends `packet` to `out` as one frame. False if it is empty, too big, or `out` is full.
pub fn encode(packet: &[u8], out: &mut Vec<u8>) -> bool {
    if packet.is_empty() || packet.len() > FRAME_MAX || out.len() + 2 + packet.len() > BACKLOG {
        return false;
    }
    out.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    out.extend_from_slice(packet);
    true
}

/// One stream to the server: framed reads and a bounded write backlog.
pub struct Conn {
    pub socket: TcpStream,
    framer: Framer,
    out: Vec<u8>,
}

impl Conn {
    pub fn new(socket: TcpStream) -> Conn {
        Conn { socket, framer: Framer::default(), out: Vec::new() }
    }

    /// Reads what is waiting and hands each whole frame to `frame`. False once the stream is
    /// over: the server closed it, it failed, or it sent a bad frame.
    pub fn read(&mut self, mut frame: impl FnMut(Vec<u8>)) -> bool {
        let mut buf = [0; CHUNK];
        loop {
            let n = match self.socket.read(&mut buf) {
                Ok(0) => return false,
                Ok(n) => n,
                Err(err) if err.kind() == ErrorKind::WouldBlock => return true,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return false,
            };
            self.framer.push(&buf[..n]);
            loop {
                match self.framer.next_frame() {
                    Ok(Some(packet)) => frame(packet),
                    Ok(None) => break,
                    Err(BadFrame) => return false,
                }
            }
        }
    }

    /// Queues one packet and writes what it can. An error ends the stream.
    pub fn send(&mut self, packet: &[u8]) -> io::Result<()> {
        // A full backlog drops the packet, as a full UDP buffer would.
        encode(packet, &mut self.out);
        self.flush()
    }

    /// Writes what it can of the backlog.
    pub fn flush(&mut self) -> io::Result<()> {
        while !self.out.is_empty() {
            match self.socket.write(&self.out) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(n) => drop(self.out.drain(..n)),
                Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                // The connect is still in flight; it says so by becoming writable.
                Err(err) if err.kind() == ErrorKind::NotConnected => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_across_reads_come_out_whole() {
        let mut out = Vec::new();
        assert!(encode(b"hello", &mut out));
        assert!(encode(b"there", &mut out));
        let mut framer = Framer::default();
        let (first, rest) = out.split_at(4);
        framer.push(first);
        assert_eq!(framer.next_frame(), Ok(None));
        framer.push(rest);
        assert_eq!(framer.next_frame(), Ok(Some(b"hello".to_vec())));
        assert_eq!(framer.next_frame(), Ok(Some(b"there".to_vec())));
        assert_eq!(framer.next_frame(), Ok(None));
    }

    #[test]
    fn an_empty_or_oversized_frame_is_refused_and_a_full_backlog_drops() {
        let mut framer = Framer::default();
        framer.push(&[0, 0]);
        assert_eq!(framer.next_frame(), Err(BadFrame));
        let mut framer = Framer::default();
        framer.push(&(FRAME_MAX as u16 + 1).to_be_bytes());
        assert_eq!(framer.next_frame(), Err(BadFrame));
        assert!(!encode(&[], &mut Vec::new()));
        assert!(!encode(&[0; FRAME_MAX + 1], &mut Vec::new()));
        let mut full = vec![0; BACKLOG - 10];
        assert!(!encode(&[1; 9], &mut full));
        assert!(encode(&[1; 8], &mut full));
    }
}
