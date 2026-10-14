//! RTP as Discord's voice servers speak it: the two `rtpsize` transport modes, the packet header,
//! and the UDP address discovery that precedes them.
//!
//! In an `rtpsize` mode the RTP header (with CSRCs and the 4-byte extension preamble, but not the
//! extension body) is authenticated and not encrypted; the nonce is a 32-bit counter, sent after
//! the tag and zero-padded to the cipher's nonce size.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::XChaCha20Poly1305;

pub const AES_GCM: &str = "aead_aes256_gcm_rtpsize";
pub const XCHACHA: &str = "aead_xchacha20_poly1305_rtpsize";
/// Opus, as Discord numbers it.
pub const PAYLOAD_TYPE: u8 = 120;
/// What a sender emits when it stops talking, so the far decoder does not interpolate into noise.
pub const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];
const HEADER: usize = 12;
const TAG: usize = 16;
const COUNTER: usize = 4;

/// The mode to ask for: AES-GCM when the server offers it, else the one every server must.
pub fn choose_mode(offered: &[String]) -> Option<&'static str> {
    [AES_GCM, XCHACHA].into_iter().find(|mode| offered.iter().any(|o| o == mode))
}

pub enum Cipher {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RtpError {
    /// Not RTP version 2, or shorter than its own header says.
    Malformed,
    /// RTCP, which shares the socket.
    Control,
    /// The tag did not verify.
    Crypto,
}

/// One received audio packet, decrypted.
#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    /// The Opus frame, still end-to-end encrypted if the call uses DAVE.
    pub payload: Vec<u8>,
}

impl Cipher {
    pub fn new(mode: &str, key: &[u8]) -> Option<Cipher> {
        match mode {
            AES_GCM => Aes256Gcm::new_from_slice(key).ok().map(|c| Cipher::Aes(Box::new(c))),
            XCHACHA => XChaCha20Poly1305::new_from_slice(key).ok().map(|c| Cipher::XChaCha(Box::new(c))),
            _ => None,
        }
    }

    fn seal(&self, counter: [u8; COUNTER], payload: Payload) -> Option<Vec<u8>> {
        match self {
            Cipher::Aes(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.encrypt(&nonce.into(), payload).ok()
            }
            Cipher::XChaCha(cipher) => {
                let mut nonce = [0u8; 24];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.encrypt(&nonce.into(), payload).ok()
            }
        }
    }

    fn open(&self, counter: [u8; COUNTER], payload: Payload) -> Option<Vec<u8>> {
        match self {
            Cipher::Aes(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[..COUNTER].copy_from_slice(&counter);
                cipher.decrypt(&nonce.into(), payload).ok()
            }
            Cipher::XChaCha(cipher) => {
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
        if (200..=204).contains(&datagram[1]) {
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
    counter: u32,
}

impl Sender {
    pub fn new(cipher: Cipher, ssrc: u32, sequence: u16, timestamp: u32) -> Sender {
        Sender { cipher, ssrc, sequence, timestamp, counter: 0 }
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
        let counter = self.counter.to_be_bytes();
        let sealed = self.cipher.seal(counter, Payload { msg: opus, aad: &datagram })?;
        datagram.extend_from_slice(&sealed);
        datagram.extend_from_slice(&counter);
        self.sequence = self.sequence.wrapping_add(1);
        self.counter = self.counter.wrapping_add(1);
        Some(datagram)
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
        let counter = [0, 0, 1, 0];
        let sealed = cipher.seal(counter, Payload { msg: &body, aad: &datagram }).unwrap();
        datagram.extend_from_slice(&sealed);
        datagram.extend_from_slice(&counter);
        let opened = cipher.unseal(&datagram).unwrap();
        assert_eq!(opened.payload, b"frame");
        assert_eq!((opened.sequence, opened.ssrc), (5, 42));
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
