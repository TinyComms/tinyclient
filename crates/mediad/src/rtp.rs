//! RTP as Discord's voice servers speak it: the two `rtpsize` transport modes, the packet header,
//! and the UDP address discovery that precedes them.
//!
//! In an `rtpsize` mode the RTP header (with CSRCs and the 4-byte extension preamble, but not the
//! extension body) is authenticated and not encrypted; the nonce is a 32-bit counter, sent after
//! the tag and zero-padded to the cipher's nonce size.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::XChaCha20Poly1305;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const AES_GCM: &str = "aead_aes256_gcm_rtpsize";
pub const XCHACHA: &str = "aead_xchacha20_poly1305_rtpsize";
/// Opus, as Discord numbers it.
pub const PAYLOAD_TYPE: u8 = 120;
/// H.264 constrained baseline, the one picture codec this client sends and decodes.
pub const H264: u8 = 103;
/// Retransmission of an H.264 packet. The payload is the original sequence, then the original bytes.
pub const RTX: u8 = 104;
/// One-byte RTP extension ids Discord's video uses.
pub const EXT_PLAYOUT: u8 = 6;
pub const EXT_CONTENT: u8 = 7;
pub const EXT_RID: u8 = 11;
/// The RID of a packet an RTX stream repaired.
pub const EXT_REPAIRED: u8 = 12;
/// What a sender emits when it stops talking, so the far decoder does not interpolate into noise.
pub const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];
const HEADER: usize = 12;
const TAG: usize = 16;
const COUNTER: usize = 4;

/// The mode to ask for: AES-GCM when the server offers it, else the one every server must.
pub fn choose_mode(offered: &[String]) -> Option<&'static str> {
    [AES_GCM, XCHACHA].into_iter().find(|mode| offered.iter().any(|o| o == mode))
}

enum Algorithm {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

struct Key {
    algorithm: Algorithm,
    /// u32::MAX + 1 is exhausted. Never wrap the wire's 32-bit nonce back to zero.
    next: AtomicU64,
}

/// One negotiated transport key. Clones share its cipher and outbound nonce allocator, across
/// audio, video, retransmission and sender recreation. A different transport gets a new key.
#[derive(Clone)]
pub struct Cipher(Arc<Key>);

#[derive(Debug, PartialEq, Eq)]
pub enum RtpError {
    /// Not RTP version 2, or shorter than its own header says.
    Malformed,
    /// RTCP, which shares the socket.
    Control,
    /// The tag did not verify.
    Crypto,
}

/// One received packet, transport-decrypted. Audio and video share the layout.
#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload_type: u8,
    pub marker: bool,
    /// The media payload, still end-to-end encrypted when the call uses DAVE.
    pub payload: Vec<u8>,
}

/// A one-byte-header extension body (RFC 5285), padded out to a whole number of 32-bit words.
/// Each element is `(id, data)` with an id of 1 to 14 and at most 16 bytes. The 4-byte `0xBEDE`
/// preamble is not part of this: it rides in the clear with the RTP header.
pub fn one_byte(elements: &[(u8, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(id, data) in elements {
        if id == 0 || id > 14 || data.is_empty() || data.len() > 16 {
            continue;
        }
        body.push((id << 4) | (data.len() as u8 - 1));
        body.extend_from_slice(data);
    }
    while body.len() % 4 != 0 {
        body.push(0);
    }
    body
}

/// The extensions on one video packet: the layer's RID, whether it is a screen, and a zero
/// playout delay. An RTX packet names the repaired RID instead of the original.
pub fn video_extension(screen: bool, repaired: bool) -> Vec<u8> {
    let rid: &[u8] = b"100";
    let content = [u8::from(screen)];
    let delay = [0u8, 0, 0];
    let id = if repaired { EXT_REPAIRED } else { EXT_RID };
    one_byte(&[(id, rid), (EXT_CONTENT, &content), (EXT_PLAYOUT, &delay)])
}

impl Cipher {
    pub fn new(mode: &str, key: &[u8]) -> Option<Cipher> {
        let algorithm = match mode {
            AES_GCM => Aes256Gcm::new_from_slice(key).ok().map(|c| Algorithm::Aes(Box::new(c))),
            XCHACHA => XChaCha20Poly1305::new_from_slice(key).ok().map(|c| Algorithm::XChaCha(Box::new(c))),
            _ => None,
        }?;
        Some(Cipher(Arc::new(Key { algorithm, next: AtomicU64::new(0) })))
    }

    /// Reserve before encrypting: even a failed encryption consumes its nonce. Only this method
    /// seals outbound packets, so a sender cannot supply or reset the counter itself.
    fn seal(&self, payload: Payload) -> Option<(Vec<u8>, [u8; COUNTER])> {
        let mut next = self.0.next.load(Ordering::Relaxed);
        let next = loop {
            if next > u64::from(u32::MAX) {
                return None;
            }
            match self.0.next.compare_exchange_weak(next, next + 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(reserved) => break reserved,
                Err(current) => next = current,
            }
        };
        let counter = (next as u32).to_be_bytes();
        let sealed = match &self.0.algorithm {
            Algorithm::Aes(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.encrypt(&nonce.into(), payload).ok()
            }
            Algorithm::XChaCha(cipher) => {
                let mut nonce = [0u8; 24];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.encrypt(&nonce.into(), payload).ok()
            }
        }?;
        Some((sealed, counter))
    }

    /// Opens a datagram whose clear prefix is `aad` and whose sealed body is `body`, using the
    /// nonce counter that followed the tag. Video's generic NACKs use an 8-byte RTCP header.
    pub fn open_aad(&self, counter: [u8; COUNTER], aad: &[u8], body: &[u8]) -> Option<Vec<u8>> {
        self.open(counter, Payload { msg: body, aad })
    }

    fn open(&self, counter: [u8; COUNTER], payload: Payload) -> Option<Vec<u8>> {
        match &self.0.algorithm {
            Algorithm::Aes(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.decrypt(&nonce.into(), payload).ok()
            }
            Algorithm::XChaCha(cipher) => {
                let mut nonce = [0u8; 24];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.decrypt(&nonce.into(), payload).ok()
            }
        }
    }

    /// Opens one datagram from the voice server.
    pub fn unseal(&self, datagram: &[u8]) -> Result<Packet, RtpError> {
        if datagram.len() < HEADER + TAG + COUNTER || datagram[0] >> 6 != 2 {
            return Err(RtpError::Malformed);
        }
        // RTCP shares the socket. The marker bit is not part of an RTCP payload type, so the
        // whole second byte is the type. 205 is a generic NACK, which video answers on its own.
        if (200..=206).contains(&datagram[1]) {
            return Err(RtpError::Control);
        }
        let extended = datagram[0] & 0x10 != 0;
        let csrcs = usize::from(datagram[0] & 0x0f);
        let clear = HEADER + 4 * csrcs + if extended { 4 } else { 0 };
        let end = datagram.len() - COUNTER;
        if clear + TAG > end {
            return Err(RtpError::Malformed);
        }
        let counter = datagram[end..].try_into().unwrap();
        let sealed = Payload { msg: &datagram[clear..end], aad: &datagram[..clear] };
        let mut payload = self.open(counter, sealed).ok_or(RtpError::Crypto)?;
        if extended {
            // The extension's length, in 32-bit words, is the last two bytes of the clear header.
            let words = u16::from_be_bytes([datagram[clear - 2], datagram[clear - 1]]);
            let body = 4 * usize::from(words);
            if body > payload.len() {
                return Err(RtpError::Malformed);
            }
            payload.drain(..body);
        }
        Ok(Packet {
            sequence: u16::from_be_bytes([datagram[2], datagram[3]]),
            timestamp: u32::from_be_bytes(datagram[4..8].try_into().unwrap()),
            ssrc: u32::from_be_bytes(datagram[8..12].try_into().unwrap()),
            payload_type: datagram[1] & 0x7f,
            marker: datagram[1] & 0x80 != 0,
            payload,
        })
    }
}

/// Our side of the stream: the counters every packet we send carries.
pub struct Sender {
    cipher: Cipher,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
}

impl Sender {
    pub fn new(cipher: Cipher, ssrc: u32, sequence: u16, timestamp: u32) -> Sender {
        Sender { cipher, ssrc, sequence, timestamp }
    }

    /// 20 ms passed, whether or not a frame was sent in them.
    pub fn advance(&mut self, samples: u32) {
        self.timestamp = self.timestamp.wrapping_add(samples);
    }

    /// Steps back, to send frames that were captured before now at the times they were heard.
    /// Only over a stretch in which nothing was sent.
    pub fn rewind(&mut self, samples: u32) {
        self.timestamp = self.timestamp.wrapping_sub(samples);
    }

    /// The datagram for one Opus frame at the current timestamp.
    pub fn seal(&mut self, opus: &[u8]) -> Option<Vec<u8>> {
        let mut datagram = Vec::with_capacity(HEADER + opus.len() + TAG + COUNTER);
        datagram.extend_from_slice(&[0x80, PAYLOAD_TYPE]);
        datagram.extend_from_slice(&self.sequence.to_be_bytes());
        datagram.extend_from_slice(&self.timestamp.to_be_bytes());
        datagram.extend_from_slice(&self.ssrc.to_be_bytes());
        let (sealed, counter) = self.cipher.seal(Payload { msg: opus, aad: &datagram })?;
        datagram.extend_from_slice(&sealed);
        datagram.extend_from_slice(&counter);
        self.sequence = self.sequence.wrapping_add(1);
        Some(datagram)
    }
}

/// One video SSRC. Its RTP sequence is its own; transport nonces belong to the shared key.
/// The timestamp is the frame's, at 90 kHz, not a clock this type keeps.
pub struct VideoSender {
    cipher: Cipher,
    ssrc: u32,
    sequence: u16,
}

impl VideoSender {
    pub fn new(cipher: Cipher, ssrc: u32) -> VideoSender {
        VideoSender { cipher, ssrc, sequence: 0 }
    }

    /// The datagram for one packet. `extension` is the one-byte body, already padded; empty sends
    /// none. The preamble is authenticated with the header and the body is encrypted ahead of
    /// the payload. Gives the datagram and the sequence number it was stamped with.
    pub fn seal(&mut self, payload_type: u8, marker: bool, timestamp: u32, payload: &[u8], extension: &[u8]) -> Option<(Vec<u8>, u16)> {
        let extended = !extension.is_empty();
        let mut header = Vec::with_capacity(HEADER + 4 + payload.len());
        let first = 0x80 | if extended { 0x10 } else { 0 };
        let second = (payload_type & 0x7f) | if marker { 0x80 } else { 0 };
        header.push(first);
        header.push(second);
        header.extend_from_slice(&self.sequence.to_be_bytes());
        header.extend_from_slice(&timestamp.to_be_bytes());
        header.extend_from_slice(&self.ssrc.to_be_bytes());
        let mut plain = Vec::with_capacity(extension.len() + payload.len());
        if extended {
            let words = u16::try_from(extension.len() / 4).ok()?;
            header.extend_from_slice(&[0xbe, 0xde]);
            header.extend_from_slice(&words.to_be_bytes());
            plain.extend_from_slice(extension);
        }
        plain.extend_from_slice(payload);
        let sequence = self.sequence;
        let (sealed, counter) = self.cipher.seal(Payload { msg: &plain, aad: &header })?;
        header.extend_from_slice(&sealed);
        header.extend_from_slice(&counter);
        self.sequence = self.sequence.wrapping_add(1);
        Some((header, sequence))
    }
}

/// The 74-byte request that asks the voice server which address it sees us at.
pub fn discovery_request(ssrc: u32) -> [u8; 74] {
    let mut packet = [0u8; 74];
    packet[..2].copy_from_slice(&1u16.to_be_bytes());
    packet[2..4].copy_from_slice(&70u16.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    packet
}

/// Our address and port as the voice server saw them.
pub fn discovery_response(packet: &[u8]) -> Option<(String, u16)> {
    if packet.len() < 74 || packet[..2] != 2u16.to_be_bytes() {
        return None;
    }
    let address = &packet[8..72];
    let end = address.iter().position(|&b| b == 0).unwrap_or(address.len());
    let address = std::str::from_utf8(&address[..end]).ok()?.to_owned();
    Some((address, u16::from_be_bytes([packet[72], packet[73]])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_exhaustion_stops_audio_and_video_instead_of_wrapping() {
        for mode in [AES_GCM, XCHACHA] {
            let mut audio = Sender::new(Cipher::new(mode, &[3; 32]).unwrap(), 1, 0, 0);
            audio.cipher.0.next.store(u64::from(u32::MAX), Ordering::Relaxed);
            assert!(audio.seal(b"last").is_some());
            assert!(audio.seal(b"must not repeat zero").is_none(), "{mode}: audio nonce exhausted");
            let mut video = VideoSender::new(Cipher::new(mode, &[4; 32]).unwrap(), 2);
            video.cipher.0.next.store(u64::from(u32::MAX), Ordering::Relaxed);
            assert!(video.seal(H264, true, 0, b"last", &[]).is_some());
            assert!(video.seal(H264, true, 0, b"must not repeat zero", &[]).is_none(), "{mode}: video nonce exhausted");
        }
    }

    #[test]
    fn exhausting_the_key_stops_every_sender_and_recreation_cannot_reset_it() {
        for mode in [AES_GCM, XCHACHA] {
            let cipher = Cipher::new(mode, &[3; 32]).unwrap();
            cipher.0.next.store(u64::from(u32::MAX) - 1, Ordering::Relaxed);
            let mut audio = Sender::new(cipher.clone(), 1, 0, 0);
            let mut video = VideoSender::new(cipher.clone(), 2);
            let mut repair = VideoSender::new(cipher.clone(), 3);
            let audio_packet = audio.seal(b"penultimate").unwrap();
            let video_packet = video.seal(H264, true, 0, b"last", &[]).unwrap().0;
            assert_eq!(&audio_packet[audio_packet.len() - 4..], &[255, 255, 255, 254]);
            assert_eq!(&video_packet[video_packet.len() - 4..], &[255, 255, 255, 255]);
            assert!(repair.seal(RTX, true, 0, b"exhausted", &[]).is_none());
            assert!(audio.seal(b"exhausted").is_none());
            assert!(video.seal(H264, true, 0, b"exhausted", &[]).is_none());
            assert!(VideoSender::new(cipher, 4).seal(H264, true, 0, b"exhausted", &[]).is_none());
        }
    }

    #[test]
    fn simultaneous_senders_reserve_distinct_nonces() {
        for mode in [AES_GCM, XCHACHA] {
            let cipher = Cipher::new(mode, &[5; 32]).unwrap();
            let senders = (0..8).map(|ssrc| {
                let cipher = cipher.clone();
                std::thread::spawn(move || {
                    let mut sender = Sender::new(cipher, ssrc, 0, 0);
                    (0..64).map(|_| {
                        let packet = sender.seal(b"audio").unwrap();
                        u32::from_be_bytes(packet[packet.len() - 4..].try_into().unwrap())
                    }).collect::<Vec<_>>()
                })
            }).collect::<Vec<_>>();
            let nonces = senders.into_iter().flat_map(|sender| sender.join().unwrap()).collect::<std::collections::HashSet<_>>();
            assert_eq!(nonces.len(), 512, "{mode}");
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// NIST GCM test case 16 (AES-256, 96-bit IV, with AAD), from the GCM specification.
    #[test]
    fn aes256_gcm_matches_the_published_vector() {
        let key = hex("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308");
        let iv = hex("cafebabefacedbaddecaf888");
        let plain = hex("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39");
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce: [u8; 12] = iv.try_into().unwrap();
        let sealed = cipher.encrypt(&nonce.into(), Payload { msg: &plain, aad: &aad }).unwrap();
        let expected = hex("522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662");
        assert_eq!(sealed[..plain.len()], expected[..]);
        assert_eq!(sealed[plain.len()..], hex("76fc6ece0f4e1768cddf8853bb2d551b")[..]);
    }

    /// The XChaCha20-Poly1305 example of draft-irtf-cfrg-xchacha-03, appendix A.3.1.
    #[test]
    fn xchacha20_poly1305_matches_the_published_vector() {
        let key = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        let nonce: [u8; 24] = hex("404142434445464748494a4b4c4d4e4f5051525354555657").try_into().unwrap();
        let plain = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let aad = hex("50515253c0c1c2c3c4c5c6c7");
        let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
        let sealed = cipher.encrypt(&nonce.into(), Payload { msg: plain, aad: &aad }).unwrap();
        assert_eq!(sealed[..16], hex("bd6d179d3e83d43b9576579493c0e939")[..]);
        assert_eq!(sealed[plain.len()..], hex("c0875924c1c7987947deafd8780acf49")[..]);
    }

    #[test]
    fn a_sealed_frame_opens_to_the_same_frame_in_both_modes() {
        for mode in [AES_GCM, XCHACHA] {
            let key = [7u8; 32];
            let mut sender = Sender::new(Cipher::new(mode, &key).unwrap(), 0x0102_0304, 65_535, 4_000_000_000);
            let far = Cipher::new(mode, &key).unwrap();
            let first = sender.seal(b"opus frame").unwrap();
            // Layout: 12 clear bytes, the frame and its tag, then the 4-byte nonce counter.
            assert_eq!(first.len(), 12 + 10 + 16 + 4, "{mode}");
            assert_eq!(first[..2], [0x80, 120]);
            assert_eq!(first[first.len() - 4..], [0, 0, 0, 0]);
            let opened = far.unseal(&first).unwrap();
            assert_eq!(opened.payload, b"opus frame");
            assert_eq!((opened.sequence, opened.timestamp, opened.ssrc), (65_535, 4_000_000_000, 0x0102_0304));

            sender.advance(960);
            let second = sender.seal(b"next").unwrap();
            assert_eq!(second[second.len() - 4..], [0, 0, 0, 1], "the nonce counter rises");
            let opened = far.unseal(&second).unwrap();
            assert_eq!((opened.sequence, opened.timestamp), (0, 4_000_000_000u32.wrapping_add(960)), "both wrap");

            // The header is authenticated: a changed sequence number does not open.
            let mut forged = second.clone();
            forged[3] ^= 1;
            assert_eq!(far.unseal(&forged), Err(RtpError::Crypto));
            let other = Cipher::new(mode, &[8u8; 32]).unwrap();
            assert_eq!(other.unseal(&second), Err(RtpError::Crypto));
        }
    }

    /// What Discord's own clients send: a one-byte-header extension whose 4-byte preamble is in
    /// the clear and whose body is encrypted ahead of the frame.
    #[test]
    fn an_extension_body_is_stripped_and_its_preamble_authenticated() {
        let cipher = Cipher::new(AES_GCM, &[9u8; 32]).unwrap();
        let mut datagram = vec![0x90, 120, 0, 5, 0, 0, 3, 192, 0, 0, 0, 42, 0xbe, 0xde, 0, 2];
        let mut body = vec![0x10, 0xaa, 0x20, 0xbb, 0, 0, 0, 0];
        body.extend_from_slice(b"frame");
        let (sealed, counter) = cipher.seal(Payload { msg: &body, aad: &datagram }).unwrap();
        datagram.extend_from_slice(&sealed);
        datagram.extend_from_slice(&counter);
        let opened = cipher.unseal(&datagram).unwrap();
        assert_eq!(opened.payload, b"frame");
        assert_eq!((opened.sequence, opened.ssrc), (5, 42));
    }

    #[test]
    fn a_video_packet_carries_its_extension_in_the_encrypted_prefix() {
        let key = [3u8; 32];
        let mut sender = VideoSender::new(Cipher::new(AES_GCM, &key).unwrap(), 0x51c0_0001);
        let extension = video_extension(true, false);
        assert_eq!(extension.len() % 4, 0);
        let (datagram, sequence) = sender.seal(H264, true, 9_000, b"nal", &extension).unwrap();
        assert_eq!(sequence, 0);
        assert_eq!(datagram[0], 0x90, "the extension bit is set");
        assert_eq!(datagram[1], 0x80 | H264, "the marker is the high bit of the payload type");
        assert_eq!(&datagram[12..16], &[0xbe, 0xde, 0, (extension.len() / 4) as u8]);
        let far = Cipher::new(AES_GCM, &key).unwrap();
        let opened = far.unseal(&datagram).unwrap();
        assert_eq!(opened.payload, b"nal");
        assert_eq!((opened.payload_type, opened.marker, opened.ssrc), (H264, true, 0x51c0_0001));
        // The preamble is authenticated with the header. A changed word count does not open.
        let mut forged = datagram.clone();
        forged[15] ^= 1;
        assert_eq!(far.unseal(&forged), Err(RtpError::Crypto));
    }

    #[test]
    fn what_is_not_audio_is_named_not_decrypted() {
        let cipher = Cipher::new(XCHACHA, &[1u8; 32]).unwrap();
        let mut rtcp = vec![0x80, 201];
        rtcp.resize(48, 0);
        assert_eq!(cipher.unseal(&rtcp), Err(RtpError::Control));
        assert_eq!(cipher.unseal(&[0x80, 120, 0]), Err(RtpError::Malformed));
        let mut wrong_version = vec![0x40, 120];
        wrong_version.resize(48, 0);
        assert_eq!(cipher.unseal(&wrong_version), Err(RtpError::Malformed));
    }

    #[test]
    fn address_discovery_round_trips() {
        let request = discovery_request(0xdead_beef);
        assert_eq!(request[..8], [0, 1, 0, 70, 0xde, 0xad, 0xbe, 0xef]);
        let mut response = [0u8; 74];
        response[..2].copy_from_slice(&2u16.to_be_bytes());
        response[2..4].copy_from_slice(&70u16.to_be_bytes());
        response[8..19].copy_from_slice(b"203.0.113.9");
        response[72..].copy_from_slice(&50_004u16.to_be_bytes());
        assert_eq!(discovery_response(&response), Some(("203.0.113.9".to_owned(), 50_004)));
        assert_eq!(discovery_response(&request), None, "a request is not an answer");
    }

    #[test]
    fn aes_is_preferred_and_xchacha_is_the_fallback() {
        let modes = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(choose_mode(&modes(&[XCHACHA, AES_GCM, "xsalsa20_poly1305"])), Some(AES_GCM));
        assert_eq!(choose_mode(&modes(&[XCHACHA])), Some(XCHACHA));
        assert_eq!(choose_mode(&modes(&["xsalsa20_poly1305"])), None);
    }
}
