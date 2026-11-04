//! The Ogg page writer for the recorder: a voice note is Opus in Ogg (RFC 7845). Only a
//! muxing writer; reading is symphonia's (`decode`). One voice note is one logical stream of
//! mono 48 kHz Opus, and the recorder hands it one packet at a time.

use std::fs::File;
use std::io::Write;

/// Ogg's CRC runs over every byte of a page through the polynomial `0x04c11db7`, high bit
/// first: no reflection, no final xor. The checksum field stays zero while it is computed.
fn crc(page: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (at, one) in table.iter_mut().enumerate() {
        let mut value = u32::try_from(at).unwrap_or(0) << 24;
        for _ in 0..8 {
            value = if value & 0x8000_0000 != 0 { (value << 1) ^ 0x04c1_1db7 } else { value << 1 };
        }
        *one = value;
    }
    let mut crc = 0u32;
    for &byte in page {
        let top = ((crc >> 24) & 0xff) ^ u32::from(byte);
        crc = (crc << 8) ^ table[usize::try_from(top).unwrap_or(0)];
    }
    crc
}

/// The page writer of one voice note: the head pages, `packet` after `packet` (each held until
/// the next, for the last packet to go with the ending page, as a well-formed stream carries
/// it), then [`Writer::end`], which takes the writer with.
pub struct Writer {
    out: File,
    serial: u32,
    sequence: u32,
    /// Samples the stream decodes to so far, carried on each page in the source's own time.
    granule: u64,
    /// The packet not written yet: everything but the last goes out through [`Writer::packet`].
    held: Option<Vec<u8>>,
}

impl Writer {
    /// A mono voice note's stream: the head (`OpusHead`) page with the encoder look-ahead as
    /// its pre-skip, then the tags; the stream runs at the source's 48 kHz.
    pub fn create(out: File, serial: u32, pre_skip: u16) -> std::io::Result<Writer> {
        let mut head = b"OpusHead".to_vec();
        head.extend([1]); // mapping version
        head.extend([1]); // a voice note is mono
        head.extend(pre_skip.to_le_bytes());
        head.extend(crate::RATE.to_le_bytes());
        head.extend(0i16.to_le_bytes()); // output gain
        head.extend([0]); // mapping family: one stream of the above
        // Version, vendor "tinyclient", no comments — in the shape the tree's own reader
        // takes (`decode`, through symphonia's vorbis comment reader, which wants the vendor
        // length first): no version byte, as the fixture `tone.opus` is crafted.
        let tags: &[u8] = b"OpusTags\x0a\x00\x00\x00tinyclient\x00\x00\x00\x00";
        let mut writer = Writer { out, serial, sequence: 0, granule: 0, held: None };
        writer.write_page(2, &head)?;
        writer.write_page(0, tags)?;
        Ok(writer)
    }

    /// One audio packet (each takes the one before it out). The samples the packets decode to
    /// so far are `granule`: one packet is one 20 ms frame here.
    pub fn packet(&mut self, packet: &[u8]) -> std::io::Result<()> {
        self.granule += crate::FRAME as u64;
        match self.held.take() {
            Some(held) => self.write_page(0, &held),
            None => Ok(()),
        }
        .and_then(|()| {
            self.held = Some(packet.to_vec());
            Ok(())
        })
    }

    /// The last page: the still held packet on it, and the stream done at `granule`.
    pub fn end(mut self, granule: u64) -> std::io::Result<()> {
        self.granule = granule;
        // A page carries at least a segment; with nothing held, the ending page is empty.
        let held = self.held.take().unwrap_or_default();
        self.write_page(4, &held)
    }

    /// One page: our packets never pass 1275 bytes, so their lacing is a few segments, well
    /// inside a page of its own.
    fn write_page(&mut self, kind: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut page = Vec::with_capacity(payload.len() + 27 + 5);
        page.extend_from_slice(b"OggS");
        page.extend_from_slice(&[0]); // structure revision
        page.extend_from_slice(&[kind]); // continued, BOS, EOS
        page.extend_from_slice(&self.granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        self.sequence += 1;
        page.extend_from_slice(&[0, 0, 0, 0]); // the checksum, a zero while it is computed
        // A page carries at least one segment, so an empty payload takes a padding one.
        let segments = (payload.len() / 255 + usize::from(!payload.is_empty())).max(1);
        page.push(u8::try_from(segments).unwrap_or(0));
        if payload.is_empty() {
            page.push(0);
        }
        for chunk in payload.chunks(255) {
            page.push(if chunk.len() == 255 { 255 } else { chunk.len() as u8 });
        }
        page.extend_from_slice(payload);
        let sum = crc(&page).to_le_bytes();
        page[22..26].copy_from_slice(&sum);
        self.out.write_all(&page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The first page of `tone.opus`, which Apple's encoder wrote: our CRC produces exactly
    /// the field the file carries, so a page we make will be one any reader takes.
    #[test]
    fn our_crc_matches_a_page_another_encoder_wrote() {
        let bytes = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.opus")).unwrap();
        assert_eq!(&bytes[..4], b"OggS", "the fixture begins with a page");
        // The page runs to the next page marker; its own size is in the segment table.
        let next = (4..bytes.len()).find(|at| bytes[*at..].starts_with(b"OggS")).unwrap_or(bytes.len());
        let mut without = bytes[..next].to_vec();
        let carried = u32::from_le_bytes(without[22..26].try_into().unwrap());
        without[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(crc(&without), carried, "the checksum the fixture names");
        // A page of ours, laid out as `write_page` does, also self-checks.
        let mut page = b"OggS".to_vec();
        page.extend_from_slice(&[0, 2]);
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&123u32.to_le_bytes());
        page.extend_from_slice(&2u32.to_le_bytes());
        page.extend_from_slice(&[0, 0, 0, 0]);
        page.extend_from_slice(&[1, 9]);
        page.extend_from_slice(b"OpusHead\x01\x01");
        // A page `write_page` makes: the field zero while it is computed, then the sum back in.
        let sum = crc(&page).to_le_bytes();
        page[22..26].copy_from_slice(&sum);
        let back = u32::from_le_bytes(page[22..26].try_into().unwrap());
        page[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(crc(&page), back);
        assert_eq!(page.len(), 27 + 1 + 10, "one page, one segment, one packet");
    }
}
