//! H.264 as Discord carries it: RFC 6184 packetization mode 1, one access unit per timestamp.
//!
//! A frame arrives as Annex B (start codes `00 00 01` or `00 00 00 01`). Each NAL that fits in
//! one packet is sent whole. A longer one is split into FU-A fragments. The marker bit is set on
//! the last packet of the access unit. A gap in the sequence throws the access unit away and
//! waits for a keyframe, so a broken picture is never handed to the decoder.

/// The most bytes of H.264 one packet carries. The RTP header, its extension and the transport
/// tag sit outside this, and the sum stays under a normal UDP payload.
pub const MAX_PAYLOAD: usize = 1000;

const FU_A: u8 = 28;
const STAP_A: u8 = 24;

/// One packet of an access unit.
#[derive(Debug, PartialEq, Eq)]
pub struct Piece {
    pub payload: Vec<u8>,
    pub marker: bool,
}

/// True when the access unit holds an IDR picture or the parameter sets a decoder needs to start.
pub fn is_key(annex_b: &[u8]) -> bool {
    split(annex_b).into_iter().any(|nal| matches!(nal_type(nal), 5 | 7 | 8))
}

fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|byte| byte & 0x1f).unwrap_or(0)
}

/// The NALs of an Annex B buffer, without their start codes. A buffer with no start code is one NAL.
fn split(frame: &[u8]) -> Vec<&[u8]> {
    let mut codes = Vec::new();
    let mut at = 0;
    while at + 3 <= frame.len() {
        if frame[at..].starts_with(&[0, 0, 0, 1]) {
            codes.push((at, 4));
            at += 4;
        } else if frame[at..].starts_with(&[0, 0, 1]) {
            codes.push((at, 3));
            at += 3;
        } else {
            at += 1;
        }
    }
    if codes.is_empty() {
        return if frame.is_empty() { Vec::new() } else { vec![frame] };
    }
    let mut nals = Vec::new();
    for (index, &(start, len)) in codes.iter().enumerate() {
        let from = start + len;
        let to = codes.get(index + 1).map(|(next, _)| *next).unwrap_or(frame.len());
        if from < to {
            nals.push(&frame[from..to]);
        }
    }
    nals
}

fn annex(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

/// Splits one Annex B access unit into packets. `max_payload` is the most a packet may carry,
/// header included; a fragment's own two header bytes come out of it.
pub fn packetize(frame: &[u8], max_payload: usize) -> Vec<Piece> {
    let nals = split(frame);
    let mut out = Vec::new();
    let last = nals.len().saturating_sub(1);
    let room = max_payload.saturating_sub(2).max(1);
    for (index, nal) in nals.iter().enumerate() {
        let end = index == last;
        if nal.is_empty() {
            continue;
        }
        if nal.len() <= max_payload {
            out.push(Piece { payload: nal.to_vec(), marker: end });
            continue;
        }
        let indicator = (nal[0] & 0x60) | FU_A;
        let kind = nal[0] & 0x1f;
        let mut offset = 1;
        while offset < nal.len() {
            let take = (nal.len() - offset).min(room);
            let start = offset == 1;
            let finish = offset + take == nal.len();
            let mut header = kind;
            if start {
                header |= 0x80;
            }
            if finish {
                header |= 0x40;
            }
            let mut payload = vec![indicator, header];
            payload.extend_from_slice(&nal[offset..offset + take]);
            offset += take;
            out.push(Piece { payload, marker: end && finish });
        }
    }
    out
}

/// Rebuilds access units from packets of one SSRC, in sequence order.
#[derive(Debug, Default)]
pub struct Assembler {
    timestamp: Option<u32>,
    next: Option<u16>,
    nals: Vec<Vec<u8>>,
    fu: Vec<u8>,
    fu_open: bool,
    /// A packet was lost. Nothing is emitted until a keyframe begins.
    broken: bool,
}

impl Assembler {
    /// One packet. `Some` is the Annex B access unit, when this packet closed it and nothing
    /// in it was missing.
    pub fn push(&mut self, sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }
        let other_time = self.timestamp.is_some_and(|held| held != timestamp);
        let gap = self.next.is_some_and(|next| next != sequence);
        if other_time || gap {
            self.nals.clear();
            self.fu.clear();
            self.fu_open = false;
            if gap {
                self.broken = true;
            } else {
                self.broken = false;
            }
        }
        self.timestamp = Some(timestamp);
        self.next = Some(sequence.wrapping_add(1));
        if self.broken && !starts_key(payload) {
            return None;
        }
        if self.broken {
            self.broken = false;
            self.nals.clear();
            self.fu.clear();
            self.fu_open = false;
        }
        if !self.absorb(payload) {
            self.broken = true;
            self.nals.clear();
            self.fu.clear();
            self.fu_open = false;
            return None;
        }
        if marker && !self.fu_open && !self.nals.is_empty() {
            let frame = annex(&self.nals);
            self.nals.clear();
            self.fu_open = false;
            self.timestamp = None;
            return Some(frame);
        }
        None
    }

    fn absorb(&mut self, payload: &[u8]) -> bool {
        let kind = payload[0] & 0x1f;
        if kind == FU_A {
            if payload.len() < 2 {
                return false;
            }
            let header = payload[1];
            let start = header & 0x80 != 0;
            let end = header & 0x40 != 0;
            if start {
                self.fu.clear();
                self.fu.push((payload[0] & 0xe0) | (header & 0x1f));
                self.fu.extend_from_slice(&payload[2..]);
                self.fu_open = !end;
                if end {
                    self.nals.push(std::mem::take(&mut self.fu));
                }
                return true;
            }
            if !self.fu_open {
                return false;
            }
            self.fu.extend_from_slice(&payload[2..]);
            if end {
                self.fu_open = false;
                self.nals.push(std::mem::take(&mut self.fu));
            }
            return true;
        }
        if self.fu_open {
            return false;
        }
        if kind == STAP_A {
            let mut at = 1;
            let mut any = false;
            while at + 2 <= payload.len() {
                let len = usize::from(u16::from_be_bytes([payload[at], payload[at + 1]]));
                at += 2;
                if len == 0 || at + len > payload.len() {
                    return false;
                }
                self.nals.push(payload[at..at + len].to_vec());
                at += len;
                any = true;
            }
            return any;
        }
        if (1..=23).contains(&kind) {
            self.nals.push(payload.to_vec());
            return true;
        }
        false
    }
}

/// A keyframe's first packet: an IDR or an SPS, whole or the start of a fragment.
fn starts_key(payload: &[u8]) -> bool {
    let kind = payload[0] & 0x1f;
    if matches!(kind, 5 | 7) {
        return true;
    }
    kind == FU_A && payload.len() > 1 && payload[1] & 0x80 != 0 && matches!(payload[1] & 0x1f, 5 | 7)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: &[u8], max_payload: usize) -> Vec<u8> {
        let pieces = packetize(frame, max_payload);
        assert!(pieces.last().is_some_and(|piece| piece.marker), "the access unit ends marked");
        assert!(pieces[..pieces.len() - 1].iter().all(|piece| !piece.marker));
        let mut assembler = Assembler::default();
        let mut out = None;
        for (index, piece) in pieces.iter().enumerate() {
            let got = assembler.push(index as u16, 90_000, piece.marker, &piece.payload);
            if index + 1 == pieces.len() {
                out = got;
            } else {
                assert!(got.is_none(), "nothing is emitted before the marker");
            }
        }
        out.expect("the access unit came back")
    }

    #[test]
    fn a_small_access_unit_and_one_larger_than_the_packet_both_come_back() {
        let small = [0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert_eq!(round_trip(&small, MAX_PAYLOAD), small);
        assert!(is_key(&small));

        let mut nal = vec![0x65, 0x88];
        nal.extend(std::iter::repeat(0x5a).take(80));
        let mut large = vec![0, 0, 0, 1];
        large.extend_from_slice(&nal);
        let back = round_trip(&large, 20);
        assert_eq!(back, large, "FU-A rebuilds the NAL");
        assert!(packetize(&large, 20).len() > 2, "the NAL did not fit in one packet");
    }

    #[test]
    fn a_sequence_gap_waits_for_a_keyframe() {
        let mut assembler = Assembler::default();
        let sps = [0x67, 0x42, 0xe0, 0x1f];
        let slice = [0x41, 0x9a, 1, 2, 3, 4];
        assert!(assembler.push(1, 100, false, &sps).is_none());
        assert!(assembler.push(3, 100, true, &slice).is_none(), "the missing packet is not invented");
        let again = [0x67, 0x42, 0xe0, 0x1f];
        let idr = [0x65, 0x88, 0x10];
        assert!(assembler.push(4, 200, false, &again).is_none());
        let frame = assembler.push(5, 200, true, &idr).expect("the keyframe is a fresh start");
        assert_eq!(frame, [0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x65, 0x88, 0x10]);
    }
}
