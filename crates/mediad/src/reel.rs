//! The picture of a round video message, beside the audio [`crate::decode::Source`] already reads.
//!
//! Symphonia demuxes the MP4 and leaves the H.264 track as `CODEC_TYPE_NULL` (it has no video
//! decoder). The samples are length-prefixed NALs. The `avcC` box, which symphonia does not keep,
//! says how long each length is and carries the parameter sets. OpenH264, already loaded for
//! calls, decodes Annex B. A phone uses its own decoder. Frames are JPEGs, one generation apart,
//! at about ten a second: a round note is about 400 px and at most a minute.

use crate::picture::{self, Planes};
use crate::platform::{PixelFormat, VideoFrame};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use openh264::formats::YUVSource;
use protocol::{annex_b_from_avcc, avc_config, PlayerEvent};
use std::fs::File;
use std::path::{Path, PathBuf};
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

const UNREADABLE: &str = "This video message can't be played here.";
/// How far ahead of the sound a frame may be decoded, and how often one is written.
const SLACK_MS: u32 = 40;
const FRAME_MS: u32 = 100;
/// A round note is a few hundred kilobytes. Past this the picture is not scanned for `avcC`.
const SCAN_LIMIT: u64 = 8 * 1024 * 1024;

trait RgbDecode {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>, String>;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct Desk(openh264::decoder::Decoder);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl RgbDecode for Desk {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
        let Some(decoded) = self.0.decode(annex_b).map_err(|err| err.to_string())? else {
            return Ok(None);
        };
        let (width, height) = decoded.dimensions();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let mut colors = vec![0u8; width * height * 3];
        decoded.write_rgb8(&mut colors);
        Ok(Some((width as u32, height as u32, colors)))
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
struct Phone(Box<dyn crate::platform::H264Decoder>);

#[cfg(any(target_os = "android", target_os = "ios"))]
impl RgbDecode for Phone {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
        let Some(frame) = self.0.decode(annex_b)? else {
            return Ok(None);
        };
        rgb_of(&frame).map(Some).ok_or_else(|| UNREADABLE.to_owned())
    }
}

fn rgb_of(frame: &VideoFrame) -> Option<(u32, u32, Vec<u8>)> {
    if !frame.is_whole() || frame.width == 0 || frame.height == 0 {
        return None;
    }
    let (y, u, v) = match frame.format {
        PixelFormat::I420 => {
            let y_len = frame.width as usize * frame.height as usize;
            let chroma = y_len / 4;
            (frame.data[..y_len].to_vec(), frame.data[y_len..y_len + chroma].to_vec(), frame.data[y_len + chroma..].to_vec())
        }
        PixelFormat::Nv12 => {
            let y_len = frame.width as usize * frame.height as usize;
            let mut u = Vec::with_capacity(y_len / 4);
            let mut v = Vec::with_capacity(y_len / 4);
            for pair in frame.data[y_len..].chunks_exact(2) {
                u.push(pair[0]);
                v.push(pair[1]);
            }
            (frame.data[..y_len].to_vec(), u, v)
        }
        PixelFormat::Rgba | PixelFormat::Bgra => {
            let bgra = frame.format == PixelFormat::Bgra;
            let mut colors = Vec::with_capacity(frame.data.len() / 4 * 3);
            for pixel in frame.data.chunks_exact(4) {
                if bgra {
                    colors.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                } else {
                    colors.extend_from_slice(&pixel[..3]);
                }
            }
            return Some((frame.width, frame.height, colors));
        }
    };
    let planes = Planes { width: frame.width as usize, height: frame.height as usize, y, u, v };
    Some((frame.width, frame.height, picture::rgb(&planes)))
}

fn open_decoder() -> Result<Box<dyn RgbDecode>, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        picture::open_h264().map(|decoder| Box::new(Desk(decoder)) as Box<dyn RgbDecode>)
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        crate::platform::required()?.h264_decoder().map(|decoder| Box::new(Phone(decoder)) as Box<dyn RgbDecode>)
    }
}

/// The `avcC` box payload, found by its name. Symphonia drops the video sample entry.
fn find_avcc(path: &Path) -> Result<protocol::AvcConfig, String> {
    let file = File::open(path).map_err(|_| UNREADABLE.to_owned())?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len == 0 || len > SCAN_LIMIT {
        return Err(UNREADABLE.to_owned());
    }
    let bytes = std::fs::read(path).map_err(|_| UNREADABLE.to_owned())?;
    let mut at = 4;
    while at + 4 < bytes.len() {
        if &bytes[at..at + 4] == b"avcC" {
            let size = u32::from_be_bytes([bytes[at - 4], bytes[at - 3], bytes[at - 2], bytes[at - 1]]) as usize;
            if size >= 8 && at + size - 4 <= bytes.len() {
                if let Some(config) = avc_config(&bytes[at + 4..at + size - 4]) {
                    return Ok(config);
                }
            }
        }
        at += 1;
    }
    Err(UNREADABLE.to_owned())
}

pub struct Reel {
    format: Box<dyn FormatReader>,
    track: u32,
    time_base: TimeBase,
    nal_length: usize,
    sets: Vec<u8>,
    decoder: Box<dyn RgbDecode>,
    pending: Option<(u32, Vec<u8>)>,
    /// The next sample after a seek carries the parameter sets again.
    need_sets: bool,
    latest: Option<(u32, u32, Vec<u8>)>,
    /// Set when `latest` is newer than the JPEG on disk.
    fresh: bool,
    generation: u64,
    last_emit: u32,
    shown: Option<PathBuf>,
}

impl Drop for Reel {
    fn drop(&mut self) {
        if let Some(path) = self.shown.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Reel {
    pub fn open(path: &Path) -> Result<Reel, String> {
        let config = find_avcc(path)?;
        let file = File::open(path).map_err(|_| UNREADABLE.to_owned())?;
        let mut hint = Hint::new();
        hint.with_extension("mp4");
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let probed = symphonia::default::get_probe()
            .format(&hint, stream, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
            .map_err(|_| UNREADABLE.to_owned())?;
        let format = probed.format;
        let video = format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec == CODEC_TYPE_NULL && track.codec_params.time_base.is_some())
            .ok_or_else(|| UNREADABLE.to_owned())?;
        let track = video.id;
        let time_base = video.codec_params.time_base.ok_or_else(|| UNREADABLE.to_owned())?;
        Ok(Reel {
            format,
            track,
            time_base,
            nal_length: config.nal_length,
            sets: config.parameter_sets,
            decoder: open_decoder()?,
            pending: None,
            need_sets: true,
            latest: None,
            fresh: false,
            generation: 0,
            last_emit: 0,
            shown: None,
        })
    }

    pub fn seek(&mut self, at: u32) -> Result<(), String> {
        let time = Time::new(u64::from(at / 1000), f64::from(at % 1000) / 1000.);
        self.format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track) }).map_err(|_| UNREADABLE.to_owned())?;
        self.pending = None;
        self.need_sets = true;
        Ok(())
    }

    /// Decodes up to where the sound has reached, and writes a JPEG when one is due.
    pub fn catch_up(&mut self, heard: u32) -> Result<Option<PlayerEvent>, String> {
        loop {
            let (at, data) = if let Some(pending) = self.pending.take() {
                pending
            } else {
                match self.next_sample()? {
                    Some(sample) => sample,
                    None => break,
                }
            };
            if at > heard.saturating_add(SLACK_MS) {
                self.pending = Some((at, data));
                break;
            }
            self.decode_sample(&data)?;
        }
        self.emit(heard)
    }

    fn next_sample(&mut self) -> Result<Option<(u32, Vec<u8>)>, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(Error::ResetRequired) => return Ok(None),
                Err(_) => return Err(UNREADABLE.to_owned()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            let time = self.time_base.calc_time(packet.ts());
            let at = (time.seconds as f64 * 1000. + time.frac * 1000.).round() as u32;
            return Ok(Some((at, packet.data.to_vec())));
        }
    }

    fn decode_sample(&mut self, data: &[u8]) -> Result<(), String> {
        let Some(annex) = annex_b_from_avcc(data, self.nal_length, &self.sets, self.need_sets) else {
            return Ok(());
        };
        self.need_sets = false;
        if let Some(picture) = self.decoder.decode(&annex)? {
            self.latest = Some(picture);
            self.fresh = true;
        }
        Ok(())
    }

    fn emit(&mut self, heard: u32) -> Result<Option<PlayerEvent>, String> {
        if !self.fresh {
            return Ok(None);
        }
        if self.generation > 0 && heard.saturating_sub(self.last_emit) < FRAME_MS {
            return Ok(None);
        }
        let Some((width, height, colors)) = self.latest.as_ref() else {
            return Ok(None);
        };
        self.generation += 1;
        let Some(path) = picture::paint_rgb(colors, *width, *height, self.generation) else {
            return Ok(None);
        };
        if let Some(old) = self.shown.replace(path.clone()) {
            let _ = std::fs::remove_file(old);
        }
        self.fresh = false;
        self.last_emit = heard;
        Ok(Some(PlayerEvent::Frame {
            path: path.display().to_string(),
            width: *width,
            height: *height,
            generation: self.generation,
        }))
    }
}
