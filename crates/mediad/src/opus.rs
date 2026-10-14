//! Opus at 48 kHz stereo, through libopus as transpiled to Rust by `unsafe-libopus`. Nothing
//! outside this file touches a codec pointer.

use crate::{CHANNELS, FRAME, RATE};
use unsafe_libopus::{
    opus_decode, opus_decoder_create, opus_decoder_destroy, opus_encode, opus_encoder_create,
    opus_encoder_ctl, opus_encoder_destroy, opus_packet_get_nb_samples, OpusDecoder, OpusEncoder,
    OPUS_APPLICATION_AUDIO, OPUS_SET_BITRATE_REQUEST, OPUS_SET_INBAND_FEC_REQUEST,
    OPUS_SET_PACKET_LOSS_PERC_REQUEST,
};

/// The longest frame Opus defines is 120 ms.
pub const MAX_FRAME: usize = FRAME * 6;
/// Discord's ceiling for voice outside boosted servers.
const BITRATE: i32 = 64_000;

pub struct Encoder(*mut OpusEncoder);

// One thread owns each codec; the pointer is to plain heap state.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> Option<Encoder> {
        Encoder::at(BITRATE)
    }

    /// An encoder at `bitrate` bits a second. Low rates make libopus choose its speech (SILK)
    /// modes, which is how tests get such packets.
    pub fn at(bitrate: i32) -> Option<Encoder> {
        let mut error = 0;
        // SAFETY: libopus allocates and initializes the state, or returns null.
        let state = unsafe { opus_encoder_create(RATE as i32, CHANNELS as i32, OPUS_APPLICATION_AUDIO, &mut error) };
        if state.is_null() || error != 0 {
            return None;
        }
        // SAFETY: `state` is a live encoder and each request takes one i32.
        unsafe {
            opus_encoder_ctl!(state, OPUS_SET_BITRATE_REQUEST, bitrate);
            opus_encoder_ctl!(state, OPUS_SET_INBAND_FEC_REQUEST, 1);
            opus_encoder_ctl!(state, OPUS_SET_PACKET_LOSS_PERC_REQUEST, 10);
        }
        Some(Encoder(state))
    }

    /// One 20 ms frame of interleaved stereo in, one Opus packet out.
    pub fn encode(&mut self, pcm: &[i16], packet: &mut [u8]) -> Option<usize> {
        assert_eq!(pcm.len(), FRAME * CHANNELS);
        // SAFETY: `pcm` holds FRAME samples per channel and `packet` is ours to fill.
        let written = unsafe { opus_encode(self.0, pcm.as_ptr(), FRAME as i32, packet.as_mut_ptr(), packet.len() as i32) };
        usize::try_from(written).ok()
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by `opus_encoder_create` and dropped once.
        unsafe { opus_encoder_destroy(self.0) }
    }
}

pub struct Decoder {
    state: *mut OpusDecoder,
    /// The last packet was CELT-only, the mode whose concealment libopus is trusted with here.
    celt: bool,
    /// The last frame that came out, for concealing a loss ourselves.
    last: Vec<i16>,
    /// Losses concealed in a row.
    misses: u32,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Option<Decoder> {
        let mut error = 0;
        // SAFETY: as for the encoder.
        let state = unsafe { opus_decoder_create(RATE as i32, CHANNELS as i32, &mut error) };
        (!state.is_null() && error == 0).then_some(Decoder { state, celt: false, last: Vec::new(), misses: 0 })
    }

    /// Decodes one packet into `pcm` (room for [`MAX_FRAME`] samples per channel). `None` as the
    /// packet conceals a lost one of `lost` samples. Gives samples per channel.
    pub fn decode(&mut self, packet: Option<&[u8]>, lost: usize, pcm: &mut [i16]) -> Option<usize> {
        let room = pcm.len() / CHANNELS;
        let Some(packet) = packet else {
            return Some(self.conceal(lost.min(room), pcm));
        };
        // SAFETY: `pcm` has room for `room` samples per channel.
        let decoded = unsafe { opus_decode(self.state, packet.as_ptr(), packet.len() as i32, pcm.as_mut_ptr(), room as i32, 0) };
        let samples = usize::try_from(decoded).ok()?;
        // The table-of-contents byte's top five bits are the configuration; 16 and up are CELT.
        self.celt = packet[0] >> 3 >= 16;
        self.misses = 0;
        self.last.clear();
        self.last.extend_from_slice(&pcm[..samples * CHANNELS]);
        Some(samples)
    }

    /// Rebuilds a lost frame from the packet after it, which carries a low-rate copy when the
    /// sender uses in-band FEC (Discord's clients do, in the speech modes). `None` when `next`
    /// holds no such copy; the loss is then concealed instead.
    pub fn recover(&mut self, next: &[u8], pcm: &mut [i16]) -> Option<usize> {
        if !has_fec(next) || pcm.len() < FRAME * CHANNELS {
            return None;
        }
        // SAFETY: `pcm` has room for FRAME samples per channel, the length of the lost frame.
        let decoded = unsafe { opus_decode(self.state, next.as_ptr(), next.len() as i32, pcm.as_mut_ptr(), FRAME as i32, 1) };
        let samples = usize::try_from(decoded).ok().filter(|&samples| samples > 0)?;
        self.misses = 0;
        self.last.clear();
        self.last.extend_from_slice(&pcm[..samples * CHANNELS]);
        Some(samples)
    }

    /// Stands in for a lost packet when the one after it cannot rebuild it ([`Decoder::recover`]).
    /// After CELT, libopus conceals it. After a SILK or hybrid packet it may not:
    /// `unsafe-libopus` 0.2.0 saturates there (a test below holds it to that, so a fixed library
    /// is noticed), so the last frame is repeated, quieter each time, and the decoder is left
    /// untouched.
    fn conceal(&mut self, samples: usize, pcm: &mut [i16]) -> usize {
        self.misses += 1;
        if self.celt {
            // SAFETY: a null packet asks for concealment of `samples` per channel, which fit.
            let decoded = unsafe { opus_decode(self.state, std::ptr::null(), 0, pcm.as_mut_ptr(), samples as i32, 0) };
            if let Ok(decoded) = usize::try_from(decoded) {
                return decoded;
            }
        }
        let gain = match self.misses {
            1 => 0.5,
            2 => 0.25,
            _ => 0.,
        };
        let out = &mut pcm[..samples * CHANNELS];
        out.fill(0);
        for (out, &last) in out.iter_mut().zip(&self.last) {
            *out = (f32::from(last) * gain) as i16;
        }
        samples
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: created by `opus_decoder_create` and dropped once.
        unsafe { opus_decoder_destroy(self.state) }
    }
}

/// True when a packet carries a low-rate copy of the frame before it (SILK's LBRR). Only the
/// plain case is read: one 20 ms frame to a packet, which is what voice sends. libopus 1.3 has
/// no call for this; the bits are where RFC 6716 section 4.2.3 and 4.2.4 put them.
pub fn has_fec(packet: &[u8]) -> bool {
    let [toc, first, ..] = packet else {
        return false;
    };
    let config = toc >> 3;
    // SILK is 0 to 11 and hybrid 12 to 15; in both, the configurations alternate 10 and 20 ms
    // except SILK's 40 and 60 ms ones (every third and fourth). One frame per packet is code 0.
    let twenty = if config < 12 { config % 4 == 1 } else { config % 2 == 1 };
    if config >= 16 || !twenty || toc & 3 != 0 {
        return false;
    }
    // The first bits are the mid channel's voice flag and LBRR flag, then the side channel's.
    let stereo = toc & 4 != 0;
    first & 0x40 != 0 || (stereo && first & 0x10 != 0)
}

/// Samples per channel a packet decodes to, or `None` if it is not an Opus packet.
pub fn samples(packet: &[u8]) -> Option<usize> {
    if packet.is_empty() {
        return None;
    }
    // SAFETY: reads only the table-of-contents bytes inside `packet`.
    let samples = unsafe { opus_packet_get_nb_samples(packet.as_ptr(), packet.len() as i32, RATE as i32) };
    usize::try_from(samples).ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::SILENCE;

    fn tone(frames: usize) -> Vec<i16> {
        (0..frames * FRAME)
            .flat_map(|n| {
                let sample = (f64::from(n as u32) * 440. * std::f64::consts::TAU / f64::from(RATE)).sin() * 8000.;
                [sample as i16; 2]
            })
            .collect()
    }

    fn energy(pcm: &[i16]) -> f64 {
        (pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
    }

    #[test]
    fn a_tone_survives_encode_and_decode() {
        let (mut encoder, mut decoder) = (Encoder::new().unwrap(), Decoder::new().unwrap());
        let source = tone(10);
        let mut heard = Vec::new();
        for frame in source.chunks(FRAME * CHANNELS) {
            let mut packet = [0u8; 1500];
            let len = encoder.encode(frame, &mut packet).unwrap();
            assert!(len > 3 && len < 400, "{len} bytes for 20 ms");
            assert_eq!(samples(&packet[..len]), Some(FRAME));
            let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
            let decoded = decoder.decode(Some(&packet[..len]), 0, &mut pcm).unwrap();
            assert_eq!(decoded, FRAME);
            heard.extend_from_slice(&pcm[..decoded * CHANNELS]);
        }
        // Past the codec's start-up, the level is the tone's (8000 peak is about 5657 RMS).
        let level = energy(&heard[4 * FRAME * CHANNELS..]);
        assert!((4500. ..6800.).contains(&level), "level {level}");
    }

    #[test]
    fn a_lost_packet_is_concealed_not_silent() {
        let (mut encoder, mut decoder) = (Encoder::new().unwrap(), Decoder::new().unwrap());
        let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
        for frame in tone(5).chunks(FRAME * CHANNELS) {
            let mut packet = [0u8; 1500];
            let len = encoder.encode(frame, &mut packet).unwrap();
            decoder.decode(Some(&packet[..len]), 0, &mut pcm).unwrap();
        }
        let concealed = decoder.decode(None, FRAME, &mut pcm).unwrap();
        assert_eq!(concealed, FRAME);
        assert!(energy(&pcm[..FRAME * CHANNELS]) > 500., "concealment carries the tone on");
    }

    fn peak(pcm: &[i16]) -> u16 {
        pcm.iter().map(|s| s.unsigned_abs()).max().unwrap()
    }

    /// At 12 kbit/s libopus sends speech-mode (SILK) packets, as a low-bitrate voice channel
    /// does. Asking `unsafe-libopus` 0.2.0 to conceal a loss after one returns full-scale
    /// samples and leaves the following frames too loud, so `Decoder` conceals those itself.
    #[test]
    fn a_loss_after_a_speech_mode_packet_is_concealed_without_a_bang() {
        let (mut encoder, mut decoder) = (Encoder::at(12_000).unwrap(), Decoder::new().unwrap());
        let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
        for (n, frame) in tone(30).chunks(FRAME * CHANNELS).enumerate() {
            let mut packet = [0u8; 1500];
            let len = encoder.encode(frame, &mut packet).unwrap();
            assert!(packet[0] >> 3 < 16, "a SILK or hybrid packet");
            if n % 7 == 6 {
                let concealed = decoder.decode(None, FRAME, &mut pcm).unwrap();
                assert_eq!(concealed, FRAME);
                let level = peak(&pcm[..FRAME * CHANNELS]);
                assert!((2000..6000).contains(&level), "concealment peaks at {level}");
                continue;
            }
            let decoded = decoder.decode(Some(&packet[..len]), 0, &mut pcm).unwrap();
            let level = peak(&pcm[..decoded * CHANNELS]);
            assert!(level < 12_000, "frame {n} peaks at {level}");
        }
    }

    fn encoded(bitrate: i32, frames: usize) -> Vec<Vec<u8>> {
        let mut encoder = Encoder::at(bitrate).unwrap();
        tone(frames)
            .chunks(FRAME * CHANNELS)
            .map(|frame| {
                let mut packet = [0u8; 1500];
                let len = encoder.encode(frame, &mut packet).unwrap();
                packet[..len].to_vec()
            })
            .collect()
    }

    /// Why `conceal` does not ask libopus after a speech-mode packet. Checked again against
    /// `unsafe-libopus` 0.2.0: its concealment there returns full scale, and so does the frame
    /// after. When this test fails the library was fixed and the workaround can go.
    #[test]
    fn the_library_still_saturates_when_asked_to_conceal_after_a_speech_mode_packet() {
        let packets = encoded(12_000, 8);
        let mut error = 0;
        // SAFETY: a decoder of our own, destroyed below; `pcm` has room for the longest frame.
        unsafe {
            let raw = opus_decoder_create(RATE as i32, CHANNELS as i32, &mut error);
            let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
            for packet in &packets[..6] {
                opus_decode(raw, packet.as_ptr(), packet.len() as i32, pcm.as_mut_ptr(), MAX_FRAME as i32, 0);
            }
            assert!(peak(&pcm[..FRAME * CHANNELS]) < 9_000, "the tone peaks at 8000");
            let concealed = opus_decode(raw, std::ptr::null(), 0, pcm.as_mut_ptr(), FRAME as i32, 0);
            assert_eq!(concealed, FRAME as i32);
            assert_eq!(peak(&pcm[..FRAME * CHANNELS]), 32_767, "fixed upstream? then drop the workaround");
            opus_decoder_destroy(raw);
        }
    }

    /// With the packet after the lost one in hand, its in-band copy gives the real frame back.
    #[test]
    fn a_lost_frame_is_rebuilt_from_the_copy_in_the_next_packet() {
        for bitrate in [24_000, 64_000] {
            let packets = encoded(bitrate, 30);
            let mut decoder = Decoder::new().unwrap();
            let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
            let mut rebuilt = 0;
            for n in 0..29 {
                if n % 7 == 6 {
                    assert!(has_fec(&packets[n + 1]), "{bitrate}: the encoder sends a copy");
                    assert_eq!(decoder.recover(&packets[n + 1], &mut pcm), Some(FRAME));
                    rebuilt += 1;
                } else {
                    decoder.decode(Some(&packets[n]), 0, &mut pcm).unwrap();
                }
                let level = peak(&pcm[..FRAME * CHANNELS]);
                assert!((7_000..9_500).contains(&level), "{bitrate}: frame {n} peaks at {level}");
            }
            assert_eq!(rebuilt, 4);
        }
    }

    #[test]
    fn a_packet_without_a_copy_is_not_asked_for_one() {
        // At 12 kbit/s there is no room for the copy; music-mode packets never carry one.
        assert!(encoded(12_000, 10).iter().all(|packet| !has_fec(packet)));
        assert!(!has_fec(&SILENCE));
        assert!(!has_fec(&[]));
        let mut decoder = Decoder::new().unwrap();
        let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
        assert_eq!(decoder.recover(&SILENCE, &mut pcm), None);
    }

    #[test]
    fn the_silence_frame_is_opus_and_decodes_to_quiet() {
        assert_eq!(samples(&SILENCE), Some(FRAME));
        let mut decoder = Decoder::new().unwrap();
        let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
        let decoded = decoder.decode(Some(&SILENCE), 0, &mut pcm).unwrap();
        assert_eq!(decoded, FRAME);
        assert!(energy(&pcm[..FRAME * CHANNELS]) < 10.);
    }

    #[test]
    fn bytes_that_are_not_opus_are_refused() {
        let mut decoder = Decoder::new().unwrap();
        let mut pcm = vec![0i16; MAX_FRAME * CHANNELS];
        assert_eq!(samples(&[]), None);
        // Code-3 packet claiming more frames than fit.
        assert_eq!(decoder.decode(Some(&[0xff, 0xff, 0xff]), 0, &mut pcm), None);
    }
}
