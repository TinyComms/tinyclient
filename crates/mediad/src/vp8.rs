//! VP8 as Discord carries it: RFC 7741 RTP depacketizer, frame reassembly, and pure-Rust RFC 6386
//! video decoding (`vp8-decoder`).
//!
//! Each VP8 RTP packet starts with a variable-length payload descriptor (1 to 6 bytes) followed by
//! VP8 frame payload bytes. A frame begins on a packet with `S=1` and `PID=0` and ends on the
//! packet carrying the RTP marker bit. Sequence gaps discard the partial frame and wait for the
//! next keyframe (`frame[0] & 1 == 0`), which DAVE leaves unencrypted in its 10-byte frame header.

use crate::picture::Planes;

/// The most bytes of VP8 payload one packet carries in tests.
pub const MAX_PAYLOAD: usize = 1000;

/// One RTP packet of a VP8 frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Piece {
    pub payload: Vec<u8>,
    pub marker: bool,
}

/// Parsed RFC 7741 §4.2 VP8 payload descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    /// Non-reference frame flag (`N`).
    pub non_reference: bool,
    /// Start of VP8 partition (`S`).
    pub start: bool,
    /// Partition index (`PID`, 0..=7).
    pub partition_id: u8,
    /// Optional 7-bit or 15-bit PictureID (`I`).
    pub picture_id: Option<u16>,
    /// Optional temporal layer zero picture index (`L`).
    pub tl0_pic_idx: Option<u8>,
    /// Optional `(tid, y, keyidx)` byte when `T` or `K` is set.
    pub tid_key: Option<u8>,
}

/// Parses the RFC 7741 payload descriptor at the front of `payload`, returning the descriptor and
/// the remaining VP8 payload slice. Returns `None` on truncated packets.
pub fn parse_descriptor(payload: &[u8]) -> Option<(Descriptor, &[u8])> {
    let first = *payload.first()?;
    let x = first & 0x80 != 0;
    let non_reference = first & 0x20 != 0;
    let start = first & 0x10 != 0;
    let partition_id = first & 0x07;
    let mut at = 1usize;
    let mut picture_id = None;
    let mut tl0_pic_idx = None;
    let mut tid_key = None;

    if x {
        let ext = *payload.get(at)?;
        at += 1;
        let has_i = ext & 0x80 != 0;
        let has_l = ext & 0x40 != 0;
        let has_t = ext & 0x20 != 0;
        let has_k = ext & 0x10 != 0;
        if has_i {
            let b0 = *payload.get(at)?;
            at += 1;
            if b0 & 0x80 != 0 {
                let b1 = *payload.get(at)?;
                at += 1;
                picture_id = Some((u16::from(b0 & 0x7f) << 8) | u16::from(b1));
            } else {
                picture_id = Some(u16::from(b0));
            }
        }
        if has_l {
            tl0_pic_idx = Some(*payload.get(at)?);
            at += 1;
        }
        if has_t || has_k {
            tid_key = Some(*payload.get(at)?);
            at += 1;
        }
    }

    if at >= payload.len() {
        return None;
    }
    Some((
        Descriptor { non_reference, start, partition_id, picture_id, tl0_pic_idx, tid_key },
        &payload[at..],
    ))
}

/// True when the VP8 frame tag indicates an intra keyframe (`P == 0`, RFC 6386 §9.1).
/// DAVE leaves byte 0 (and for keyframes, bytes 0..10) unencrypted, so this works on both
/// DAVE-sealed and decrypted VP8 frames.
pub fn is_key(frame: &[u8]) -> bool {
    frame.first().is_some_and(|b| b & 0x01 == 0)
}

/// Splits one VP8 frame (clear or DAVE-sealed) into RFC 7741 RTP payloads.
pub fn packetize(frame: &[u8], max_payload: usize) -> Vec<Piece> {
    if frame.is_empty() {
        return Vec::new();
    }
    let chunk = max_payload.saturating_sub(1).max(1);
    let total = frame.len().div_ceil(chunk);
    let mut out = Vec::with_capacity(total);
    for (index, part) in frame.chunks(chunk).enumerate() {
        let mut payload = Vec::with_capacity(1 + part.len());
        // S=1, PID=0 on the first packet; S=0, PID=0 on continuation packets.
        payload.push(if index == 0 { 0x10 } else { 0x00 });
        payload.extend_from_slice(part);
        out.push(Piece { payload, marker: index + 1 == total });
    }
    out
}

/// Rebuilds whole VP8 frames from RTP packets of one SSRC in sequence order.
#[derive(Debug, Default)]
pub struct Assembler {
    timestamp: Option<u32>,
    next: Option<u16>,
    buf: Vec<u8>,
    started: bool,
    /// A packet was lost. Nothing is emitted until a keyframe starts (`S=1, PID=0, P=0`).
    broken: bool,
}

impl Assembler {
    /// Pushes one RTP packet. Returns `Some(frame)` when the marker bit closes a complete,
    /// contiguous VP8 frame.
    pub fn push(&mut self, sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Option<Vec<u8>> {
        let (desc, body) = parse_descriptor(payload)?;
        let other_time = self.timestamp.is_some_and(|held| held != timestamp);
        let gap = self.next.is_some_and(|next| next != sequence);
        if other_time || gap {
            self.buf.clear();
            self.started = false;
            if gap {
                self.broken = true;
            }
        }
        self.timestamp = Some(timestamp);
        self.next = Some(sequence.wrapping_add(1));

        let starts_frame = desc.start && desc.partition_id == 0;
        if self.broken {
            if starts_frame && is_key(body) {
                self.broken = false;
            } else {
                return None;
            }
        }

        if starts_frame {
            self.buf.clear();
            self.started = true;
        }
        if !self.started {
            return None;
        }
        self.buf.extend_from_slice(body);
        if marker {
            let frame = std::mem::take(&mut self.buf);
            self.started = false;
            self.timestamp = None;
            return (!frame.is_empty()).then_some(frame);
        }
        None
    }
}

/// Pure-Rust VP8 video decoder (`vp8-decoder`), converting each decoded frame into packed I420
/// [`Planes`] for `picture::rgb`.
pub struct Vp8Decoder {
    inner: vp8_decoder::Decoder,
}

impl Default for Vp8Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Vp8Decoder {
    pub fn new() -> Self {
        let mut inner = vp8_decoder::Decoder::new();
        // Guard against malformed headers claiming huge resolutions (cap at 4K).
        inner.set_max_pixels(3840 * 2160);
        Self { inner }
    }

    /// Decodes one raw VP8 frame. Returns `Ok(None)` for invisible/alt-ref-only frames that only
    /// update reference buffers, `Ok(Some(planes))` for displayable frames, and `Err(())` when
    /// the frame is corrupt or requires a keyframe.
    #[allow(clippy::result_unit_err)]
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<Planes>, ()> {
        let Some(picture) = self.inner.decode(frame).map_err(|_| ())? else {
            return Ok(None);
        };
        let width = (picture.width as usize) & !1;
        let height = (picture.height as usize) & !1;
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let uv_w = width / 2;
        let uv_h = height / 2;
        let mut y = Vec::with_capacity(width * height);
        for row in 0..height {
            let start = row * picture.y_stride;
            y.extend_from_slice(&picture.y[start..start + width]);
        }
        let mut u = Vec::with_capacity(uv_w * uv_h);
        let mut v = Vec::with_capacity(uv_w * uv_h);
        for row in 0..uv_h {
            let start = row * picture.uv_stride;
            u.extend_from_slice(&picture.u[start..start + uv_w]);
            v.extend_from_slice(&picture.v[start..start + uv_w]);
        }
        Ok(Some(Planes { y, u, v, width, height }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_parses_minimal_and_extended_fields() {
        // Minimal 1-byte descriptor: S=1, PID=0, payload [0x10, 0xaa, 0xbb]
        let (desc, body) = parse_descriptor(&[0x10, 0xaa, 0xbb]).unwrap();
        assert!(desc.start);
        assert_eq!(desc.partition_id, 0);
        assert_eq!(desc.picture_id, None);
        assert_eq!(body, &[0xaa, 0xbb]);

        // Extended: X=1, S=1, PID=0 | I=1, L=1, T=1, K=1 | 15-bit PictureID 0x1234 | TL0=0x55 | TID/KEY=0x99
        let pkt = [0x90, 0xf0, 0x92, 0x34, 0x55, 0x99, 0x00, 0x01];
        let (desc, body) = parse_descriptor(&pkt).unwrap();
        assert!(desc.start);
        assert_eq!(desc.picture_id, Some(0x1234));
        assert_eq!(desc.tl0_pic_idx, Some(0x55));
        assert_eq!(desc.tid_key, Some(0x99));
        assert_eq!(body, &[0x00, 0x01]);

        // 7-bit PictureID
        let pkt7 = [0x90, 0x80, 0x42, 0xfe];
        let (desc, body) = parse_descriptor(&pkt7).unwrap();
        assert_eq!(desc.picture_id, Some(0x42));
        assert_eq!(body, &[0xfe]);

        // Truncated descriptor with no payload bytes fails cleanly.
        assert!(parse_descriptor(&[0x90, 0x80]).is_none());
        assert!(parse_descriptor(&[0x10]).is_none());
    }

    #[test]
    fn assembler_reassembles_fragmented_frames_and_recovers_on_keyframe_after_gap() {
        // Fake keyframe (byte 0 bit 0 == 0) and delta frame (byte 0 bit 0 == 1).
        let mut key = vec![0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x30, 0x00];
        key.extend(std::iter::repeat(0xab).take(50));
        let mut delta = vec![0x11, 0x00, 0x00];
        delta.extend(std::iter::repeat(0xcd).take(40));

        let mut asm = Assembler::default();
        let pieces = packetize(&key, 20);
        assert!(pieces.len() > 2);
        let mut out = None;
        for (i, p) in pieces.iter().enumerate() {
            let got = asm.push(i as u16, 90_000, p.marker, &p.payload);
            if i + 1 == pieces.len() {
                out = got;
            } else {
                assert!(got.is_none());
            }
        }
        assert_eq!(out.as_deref(), Some(key.as_slice()));

        // Introduce a sequence gap: delta frame is dropped while broken.
        let delta_pieces = packetize(&delta, 100);
        assert!(asm.push(100, 93_000, delta_pieces[0].marker, &delta_pieces[0].payload).is_none());

        // Next keyframe recovers cleanly.
        let key_pieces = packetize(&key, 100);
        let recovered = asm.push(101, 96_000, key_pieces[0].marker, &key_pieces[0].payload);
        assert_eq!(recovered.as_deref(), Some(key.as_slice()));
    }

    #[test]
    fn real_vp8_keyframe_and_interframe_packetize_assemble_and_decode() {
        let (w, h) = (64usize, 48usize);
        let cfg = vp8_encoder::EncoderConfig::new(w as u32, h as u32, 30, 500_000);
        let mut enc = vp8_encoder::Encoder::new(cfg).expect("vp8 encoder");
        let frame1_y = vec![200u8; w * h];
        let frame1_u = vec![90u8; (w / 2) * (h / 2)];
        let frame1_v = vec![160u8; (w / 2) * (h / 2)];
        let mut i420_1 = Vec::with_capacity(w * h * 3 / 2);
        i420_1.extend_from_slice(&frame1_y);
        i420_1.extend_from_slice(&frame1_u);
        i420_1.extend_from_slice(&frame1_v);
        let pic1 = vp8_decoder::Picture::from_i420(&i420_1, w as u32, h as u32).unwrap();

        let key = enc.encode(&pic1, true).expect("encode keyframe").expect("keyframe emitted");
        assert!(is_key(&key.data));

        let mut i420_2 = vec![120u8; w * h];
        i420_2.extend(std::iter::repeat(128u8).take((w / 2) * (h / 2) * 2));
        let pic2 = vp8_decoder::Picture::from_i420(&i420_2, w as u32, h as u32).unwrap();
        let delta = enc.encode(&pic2, false).expect("encode interframe").expect("interframe emitted");

        let mut asm = Assembler::default();
        let mut dec = Vp8Decoder::new();
        let mut seq = 10u16;

        let mut assembled_key = None;
        for piece in packetize(&key.data, 120) {
            assembled_key = asm.push(seq, 90_000, piece.marker, &piece.payload);
            seq = seq.wrapping_add(1);
        }
        let planes = dec.decode(assembled_key.as_ref().expect("assembled keyframe")).unwrap().expect("decoded keyframe");
        assert_eq!((planes.width, planes.height), (w, h));
        // Luma should be close to 200.
        let avg_y: u64 = planes.y.iter().map(|&b| u64::from(b)).sum::<u64>() / (planes.y.len() as u64);
        assert!((170..=230).contains(&avg_y), "unexpected keyframe luma {avg_y}");

        let mut assembled_delta = None;
        for piece in packetize(&delta.data, 120) {
            assembled_delta = asm.push(seq, 93_000, piece.marker, &piece.payload);
            seq = seq.wrapping_add(1);
        }
        let planes2 = dec.decode(assembled_delta.as_ref().expect("assembled delta")).unwrap().expect("decoded delta");
        assert_eq!((planes2.width, planes2.height), (w, h));
    }
}
