//! Reads a voice message or audio file as 48 kHz stereo, a piece at a time: whatever the
//! container (Ogg, WebM, MP4, WAV, MP3, FLAC), through symphonia; Opus, which is what Discord and
//! Telegram voice messages are, through the same libopus the calls use. Nothing is read ahead
//! of what the speaker needs, so a long file costs no more memory than a short one.

use crate::audio::Resampler;
use crate::opus::{self, MAX_FRAME};
use crate::{CHANNELS, RATE};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

/// What the user is told when a file cannot be read; the reason goes to the log.
pub const UNREADABLE: &str = "This file can't be played.";

enum Codec {
    Opus(opus::Decoder),
    Other(Box<dyn Decoder>),
}

pub struct Source {
    format: Box<dyn FormatReader>,
    track: u32,
    codec: Codec,
    /// The decoder's own rate and channel count.
    rate: u32,
    time_base: Option<TimeBase>,
    duration: u32,
    resampler: Resampler,
    /// Frames at the decoder's rate still to be dropped: Opus's pre-skip at the start, and the
    /// part of the packet before the point a seek asked for.
    skip: usize,
    /// Opus's pre-skip, from its header, for when the container does not trim it itself.
    pre_skip: usize,
    /// Opus's output gain, from its header.
    gain: f32,
    /// Nothing has been decoded since opening or seeking.
    fresh: bool,
    pcm: Vec<i16>,
    stereo: Vec<f32>,
}

fn why(error: Error) -> String {
    eprintln!("mediad: {error}");
    UNREADABLE.to_owned()
}

/// The pre-skip and output gain of an `OpusHead` (RFC 7845 section 5.1), and whether its
/// channels are ones libopus can decode as one stream.
fn opus_head(head: &[u8]) -> Option<(usize, f32)> {
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return None;
    }
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize;
    let gain = i16::from_le_bytes([head[16], head[17]]);
    // Mapping family 0 is mono or stereo in one stream; the others are surround.
    if head[18] != 0 {
        return None;
    }
    Some((pre_skip, 10f32.powf(f32::from(gain) / 256. / 20.)))
}

impl Source {
    pub fn open(path: &Path) -> Result<Source, String> {
        let file = File::open(path).map_err(|err| {
            eprintln!("mediad: {}: {err}", path.display());
            "The file is no longer on this computer.".to_owned()
        })?;
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let options = FormatOptions { enable_gapless: true, ..Default::default() };
        let probed = symphonia::default::get_probe()
            .format(&hint, stream, &options, &MetadataOptions::default())
            .map_err(why)?;
        let format = probed.format;
        let (track, params) = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .map(|track| (track.id, track.codec_params.clone()))
            .ok_or_else(|| UNREADABLE.to_owned())?;
        let (codec, rate, pre_skip, gain) = if params.codec == CODEC_TYPE_OPUS {
            let (pre_skip, gain) = params.extra_data.as_deref().and_then(opus_head).ok_or_else(|| {
                eprintln!("mediad: an Opus stream this build cannot decode");
                UNREADABLE.to_owned()
            })?;
            let decoder = opus::Decoder::new().ok_or_else(|| UNREADABLE.to_owned())?;
            (Codec::Opus(decoder), RATE, pre_skip, gain)
        } else {
            let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(why)?;
            (Codec::Other(decoder), params.sample_rate.unwrap_or(RATE), 0, 1.)
        };
        let time_base = params.time_base.or_else(|| params.sample_rate.map(|rate| TimeBase::new(1, rate)));
        let duration = match (params.n_frames, time_base) {
            (Some(frames), Some(base)) => {
                let frames = frames.saturating_sub(u64::from(params.delay.unwrap_or(0)) + u64::from(params.padding.unwrap_or(0)));
                let time = base.calc_time(frames);
                (time.seconds as f64 * 1000. + time.frac * 1000.).round() as u32
            }
            _ => 0,
        };
        Ok(Source {
            format,
            track,
            codec,
            rate,
            time_base,
            duration,
            resampler: Resampler::new(rate, RATE),
            skip: 0,
            pre_skip,
            gain,
            fresh: true,
            pcm: vec![0; MAX_FRAME * CHANNELS],
            stereo: Vec::new(),
        })
    }

    /// Milliseconds, or 0 when the container does not say.
    pub fn duration(&self) -> u32 {
        self.duration
    }

    /// Appends the next piece as 48 kHz interleaved stereo. False at the end of the file.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(Error::ResetRequired) => return Ok(false),
                Err(err) => return Err(why(err)),
            };
            if packet.track_id() != self.track {
                continue;
            }
            self.stereo.clear();
            match &mut self.codec {
                Codec::Opus(decoder) => {
                    let Some(samples) = decoder.decode(Some(&packet.data), 0, &mut self.pcm) else {
                        // One damaged packet is a moment of silence, not the end.
                        continue;
                    };
                    let gain = self.gain / 32768.;
                    self.stereo.extend(self.pcm[..samples * CHANNELS].iter().map(|&s| f32::from(s) * gain));
                }
                Codec::Other(decoder) => {
                    let decoded = match decoder.decode(&packet) {
                        Ok(decoded) => decoded,
                        Err(Error::DecodeError(err)) => {
                            eprintln!("mediad: skipped a packet: {err}");
                            continue;
                        }
                        Err(err) => return Err(why(err)),
                    };
                    let spec = *decoded.spec();
                    let channels = spec.channels.count().max(1);
                    let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                    buffer.copy_interleaved_ref(decoded);
                    // Mono is heard in both ears; of more than two channels the first two count.
                    for frame in buffer.samples().chunks_exact(channels) {
                        self.stereo.push(frame[0]);
                        self.stereo.push(frame[if channels > 1 { 1 } else { 0 }]);
                    }
                }
            }
            // The container's trimming (gapless playback): the encoder's delay and padding.
            let mut start = packet.trim_start as usize;
            if self.fresh && start == 0 && packet.ts == 0 {
                start = self.pre_skip;
            }
            self.fresh = false;
            let frames = self.stereo.len() / CHANNELS;
            let end = frames.saturating_sub(packet.trim_end as usize);
            // What is still to be dropped past this packet is dropped from the next.
            let wanted = start + std::mem::take(&mut self.skip);
            let start = wanted.min(end);
            self.skip = wanted - start;
            self.resampler.push(&self.stereo[start * CHANNELS..end * CHANNELS], out);
            return Ok(true);
        }
    }

    /// Moves to `at` milliseconds. Gives where reading now starts.
    pub fn seek(&mut self, at: u32) -> Result<u32, String> {
        let time = Time::new(u64::from(at / 1000), f64::from(at % 1000) / 1000.);
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track) })
            .map_err(why)?;
        match &mut self.codec {
            Codec::Opus(decoder) => *decoder = opus::Decoder::new().ok_or_else(|| UNREADABLE.to_owned())?,
            Codec::Other(decoder) => decoder.reset(),
        }
        self.resampler = Resampler::new(self.rate, RATE);
        self.fresh = false;
        // A seek lands on a packet at or before the time asked for; the rest of the way is
        // decoded and dropped, so what is heard starts where the user pointed.
        let behind = seeked.required_ts.saturating_sub(seeked.actual_ts);
        self.skip = match self.time_base {
            Some(base) => {
                let time = base.calc_time(behind);
                ((time.seconds as f64 + time.frac) * f64::from(self.rate)) as usize
            }
            None => 0,
        };
        Ok(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn all(source: &mut Source) -> Vec<f32> {
        let mut out = Vec::new();
        while source.read(&mut out).unwrap() {}
        out
    }

    /// Rising zero crossings of the left channel a second.
    fn pitch(pcm: &[f32]) -> f32 {
        let left: Vec<f32> = pcm.chunks_exact(2).map(|frame| frame[0]).collect();
        let rising = left.windows(2).filter(|pair| pair[0] < 0. && pair[1] >= 0.).count();
        rising as f32 * 48_000. / left.len() as f32
    }

    fn loudness(pcm: &[f32]) -> f32 {
        (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt()
    }

    /// `tone.opus` is one second of a quiet 440 Hz tone, mono, made by Apple's encoder
    /// (`afconvert -f Oggf -d opus`): an Ogg Opus file as Telegram and Discord send them.
    #[test]
    fn an_ogg_opus_voice_message_decodes_to_its_tone_and_length() {
        let mut source = Source::open(&fixture("tone.opus")).unwrap();
        assert!((980..=1010).contains(&source.duration()), "{} ms", source.duration());
        let pcm = all(&mut source);
        let seconds = pcm.len() as f32 / 2. / 48_000.;
        assert!((0.98..1.02).contains(&seconds), "{seconds} s");
        let middle = &pcm[2 * 4_800..2 * 43_200];
        let heard = pitch(middle);
        assert!((430. ..450.).contains(&heard), "{heard} Hz");
        let level = loudness(middle);
        assert!((0.05..0.2).contains(&level), "level {level}");
        // Mono is the same in both ears.
        assert!(middle.chunks_exact(2).all(|frame| frame[0] == frame[1]));
    }

    #[test]
    fn a_seek_starts_where_it_was_asked_to() {
        let mut source = Source::open(&fixture("tone.opus")).unwrap();
        assert_eq!(source.seek(600), Ok(600));
        let rest = all(&mut source).len() as f32 / 2. / 48_000.;
        assert!((0.38..0.42).contains(&rest), "{rest} s left after 0.6 s");
    }

    /// The same tone as AAC in MP4 (Signal's voice notes, most phones' recordings), at 44.1
    /// kHz, which is resampled on the way.
    #[test]
    fn an_aac_file_is_read_and_brought_to_48_khz() {
        let mut source = Source::open(&fixture("tone.m4a")).unwrap();
        let pcm = all(&mut source);
        let seconds = pcm.len() as f32 / 2. / 48_000.;
        assert!((0.95..1.1).contains(&seconds), "{seconds} s");
        let heard = pitch(&pcm[2 * 4_800..2 * 43_200]);
        assert!((430. ..450.).contains(&heard), "{heard} Hz");
    }

    #[test]
    fn what_is_not_audio_is_refused_in_words() {
        let not_audio = fixture("../../Cargo.toml");
        assert_eq!(Source::open(&not_audio).err().as_deref(), Some(UNREADABLE));
        let missing = Source::open(Path::new("/nonexistent/voice.ogg")).err().unwrap();
        assert_eq!(missing, "The file is no longer on this computer.");
    }

    #[test]
    fn an_opus_header_gives_its_pre_skip_and_gain() {
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 2]); // version, channels
        head.extend(312u16.to_le_bytes());
        head.extend(48_000u32.to_le_bytes());
        head.extend(0i16.to_le_bytes());
        head.push(0);
        assert_eq!(opus_head(&head), Some((312, 1.)));
        head[18] = 1;
        assert_eq!(opus_head(&head), None, "surround is refused");
        assert_eq!(opus_head(b"OpusTags"), None);
    }
}
