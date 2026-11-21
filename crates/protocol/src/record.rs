//! Recording a voice message: what the shell asks, what it is told, and the frames between
//! chatd and the `mediad --rec` that does it.
//!
//! One recording at a time, across every network. chatd decodes and encodes nothing: it starts
//! a mediad in its recording mode for a file in the media cache, and the process stops when
//! chatd closes its stdin, so a finished or abandoned recording holds no device at all.

use crate::Key;
use serde::{Deserialize, Serialize};

/// What the shell shows of the recording in the composer's foot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// Where the message will go, so a recording is the composer's to show and send.
    pub conversation: Key,
    /// Unix milliseconds the recording started at, for the clock in the composer.
    pub since: u64,
    /// The microphone in use while recording, once opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Why the microphone in use is not the one chosen. `None` when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// A round video message, not a voice note.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub video: bool,
    /// Square side in pixels when `video`.
    #[serde(default, skip_serializing_if = "crate::is_zero")]
    pub side: u32,
    /// Latest JPEG preview, while a video note records. A path the shell can draw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(flatten)]
    pub state: RecordState,
}

/// What the shell knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecordState {
    /// mediad is starting, or the microphone is recording.
    Active,
    /// Done: the file is in the media cache for the shell to send as a voice note.
    Recorded {
        /// The path among the cache, named in `OutgoingFile::path`.
        path: String,
        /// Milliseconds of sound, as the recorder finished the file.
        duration: u32,
        /// A voice message's loudness over time, one byte a bar, at most
        /// [`crate::WAVEFORM_LIMIT`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waveform: Vec<u8>,
    },
    /// It cannot record, in plain words.
    Failed { detail: String },
}

/// The shell to chatd: `Request::Record`. Each is answered `Ok` at once; what happens is told
/// as `Change::Recording`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum RecordRequest {
    /// Starts one recording for the conversation: chatd names the file, mediad hears the
    /// microphone. Whatever was recording is stopped.
    Start {
        conversation: Key,
        /// The chosen microphone (Settings › Voice), or the chosen camera for a video note.
        /// `None` is automatic.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// Record a round video message (`mediad --rec-video`) instead of a voice note.
        #[serde(default, skip_serializing_if = "crate::is_false")]
        video: bool,
    },
    /// Ends the recording: the facts arrive as `Recorded`, and stay until `Forget`.
    Stop,
    /// The composer is not showing a recording: the facts and the file go.
    Forget,
}

/// `mediad --rec` to chatd, over its stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "recorder", rename_all = "snake_case")]
pub enum RecEvent {
    /// The microphone is open and recording. `device` is the microphone in use; `notice` is set
    /// when the chosen microphone was missing and recording fell back to another.
    Started {
        device: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
    },
    /// One preview frame of a round video message. Does not finish the take.
    Frame { path: String },
    /// The file is complete. mediad exits.
    Done { duration: u32, waveform: Vec<u8> },
    /// The microphone could not be opened, or the file could not be written. mediad exits.
    Failed { detail: String },
}

/// Square side of a round video message, in pixels.
pub const ROUND_SIDE: u32 = 384;
/// A round video message stops at one minute.
pub const ROUND_LIMIT_MS: u32 = 60_000;
/// Frames a second the recorder aims for.
pub const ROUND_FPS: u32 = 15;

/// `side` and `duration_ms` brought inside the round-video limits. The side stays even.
pub fn clamp_round(side: u32, duration_ms: u32) -> (u32, u32) {
    let side = if side == 0 { ROUND_SIDE } else { side.min(ROUND_SIDE) };
    let side = (side & !1).max(2);
    (side, duration_ms.min(ROUND_LIMIT_MS))
}

/// What a round video message says on Telegram's document attribute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundWire {
    pub round_message: bool,
    pub nosound: bool,
    pub mime: &'static str,
    pub side: i32,
    /// Seconds, as `documentAttributeVideo.duration` takes them.
    pub duration_secs: f64,
}

/// The attribute for `note`. A note with no AAC track is `nosound`. The side and the length
/// are clamped to what a round video message is.
pub fn round_wire(note: &crate::VideoNote) -> RoundWire {
    let (side, duration_ms) = clamp_round(note.side, note.duration);
    RoundWire {
        round_message: true,
        nosound: !note.sound,
        mime: "video/mp4",
        side: side as i32,
        duration_secs: f64::from(duration_ms) / 1000.0,
    }
}

/// One finished round video file: ISO BMFF, video only (`avc1`), samples as AVCC.
pub struct RoundFile {
    pub bytes: Vec<u8>,
    pub duration_ms: u32,
    pub samples: u32,
    pub side: u32,
}

/// Builds a video-only MP4 from Annex B access units, one frame each. Parameter sets (SPS, PPS)
/// go in `avcC`. Other NAL units are samples, each with a 4-byte length. There is no sound track:
/// this build has no AAC encoder, and Opus in an MP4 is not what Telegram's apps send.
pub fn round_mp4(side: u32, frames: &[Vec<u8>], frame_ms: u32) -> RoundFile {
    let (side, _) = clamp_round(side, 0);
    let frame_ms = frame_ms.max(1);
    let mut sps: Vec<u8> = Vec::new();
    let mut pps: Vec<u8> = Vec::new();
    let mut samples: Vec<Vec<u8>> = Vec::new();
    let mut keys: Vec<u32> = Vec::new();
    for frame in frames {
        let mut sample = Vec::new();
        let mut key = false;
        for nal in annex_b_nals(frame) {
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1f {
                7 if sps.is_empty() => sps = nal.to_vec(),
                8 if pps.is_empty() => pps = nal.to_vec(),
                7 | 8 => {}
                kind => {
                    key |= kind == 5;
                    let len = nal.len() as u32;
                    sample.extend_from_slice(&len.to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }
        if !sample.is_empty() {
            if key || samples.is_empty() {
                keys.push(samples.len() as u32 + 1);
            }
            samples.push(sample);
        }
    }
    if sps.is_empty() {
        sps = vec![0x67, 0x42, 0x00, 0x1f];
    }
    if pps.is_empty() {
        pps = vec![0x68, 0xce, 0x38, 0x80];
    }
    if keys.is_empty() {
        keys.push(1);
    }
    let duration_ms = (samples.len() as u32).saturating_mul(frame_ms).min(ROUND_LIMIT_MS);
    let bytes = assemble_mp4(side, frame_ms, duration_ms, &sps, &pps, &samples, &keys);
    RoundFile { bytes, duration_ms, samples: samples.len() as u32, side }
}

fn annex_b_nals(bytes: &[u8]) -> Vec<&[u8]> {
    let mut codes = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if i + 4 <= bytes.len() && bytes[i..i + 4] == [0, 0, 0, 1] {
            codes.push((i, 4));
            i += 4;
        } else if bytes[i..i + 3] == [0, 0, 1] {
            codes.push((i, 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    if codes.is_empty() {
        return if bytes.is_empty() { Vec::new() } else { vec![bytes] };
    }
    let mut nals = Vec::with_capacity(codes.len());
    for (n, (at, len)) in codes.iter().copied().enumerate() {
        let start = at + len;
        let end = codes.get(n + 1).map(|(next, _)| *next).unwrap_or(bytes.len());
        if start < end {
            nals.push(&bytes[start..end]);
        }
    }
    nals
}

fn assemble_mp4(side: u32, frame_ms: u32, duration_ms: u32, sps: &[u8], pps: &[u8], samples: &[Vec<u8>], keys: &[u32]) -> Vec<u8> {
    let ftyp = box4(b"ftyp", &{
        let mut body = Vec::new();
        body.extend_from_slice(b"isom");
        body.extend_from_slice(&0u32.to_be_bytes());
        for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
            body.extend_from_slice(brand);
        }
        body
    });
    let mut mdat_body = Vec::new();
    for sample in samples {
        mdat_body.extend_from_slice(sample);
    }
    let mdat = box4(b"mdat", &mdat_body);
    let data_at = (ftyp.len() + 8) as u32;
    let avcc = avc_decoder_config(sps, pps);
    let avc1 = box4(b"avc1", &{
        let mut body = vec![0u8; 78];
        body[6..8].copy_from_slice(&1u16.to_be_bytes());
        body[24..26].copy_from_slice(&(side as u16).to_be_bytes());
        body[26..28].copy_from_slice(&(side as u16).to_be_bytes());
        body[28..32].copy_from_slice(&0x00480000u32.to_be_bytes());
        body[32..36].copy_from_slice(&0x00480000u32.to_be_bytes());
        body[40..42].copy_from_slice(&1u16.to_be_bytes());
        body[74..76].copy_from_slice(&0x0018u16.to_be_bytes());
        body[76..78].copy_from_slice(&0xffffu16.to_be_bytes());
        body.extend_from_slice(&avcc);
        body
    });
    let stsd = full_box(b"stsd", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&avc1);
        body
    });
    let stts = full_box(b"stts", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for _ in samples {
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&frame_ms.to_be_bytes());
        }
        body
    });
    let stsc = full_box(b"stsc", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&(samples.len().max(1) as u32).to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body
    });
    let stsz = full_box(b"stsz", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for sample in samples {
            body.extend_from_slice(&(sample.len() as u32).to_be_bytes());
        }
        body
    });
    let stco = full_box(b"stco", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&data_at.to_be_bytes());
        body
    });
    let stss = full_box(b"stss", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&(keys.len() as u32).to_be_bytes());
        for key in keys {
            body.extend_from_slice(&key.to_be_bytes());
        }
        body
    });
    let stbl = box4(b"stbl", &[stsd, stts, stsc, stsz, stco, stss].concat());
    let vmhd = full_box(b"vmhd", 1, &[0, 0, 0, 0, 0, 0, 0, 0]);
    let url = full_box(b"url ", 1, &[]);
    let dref = full_box(b"dref", 0, &{
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&url);
        body
    });
    let dinf = box4(b"dinf", &dref);
    let minf = box4(b"minf", &[vmhd, dinf, stbl].concat());
    let mdhd = full_box(b"mdhd", 0, &{
        let mut body = vec![0u8; 16];
        body[8..12].copy_from_slice(&1000u32.to_be_bytes());
        body[12..16].copy_from_slice(&duration_ms.to_be_bytes());
        body.extend_from_slice(&0x55c4u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body
    });
    let hdlr = full_box(b"hdlr", 0, &{
        let mut body = vec![0u8; 20];
        body[4..8].copy_from_slice(b"vide");
        body.extend_from_slice(b"VideoHandler\0");
        body
    });
    let mdia = box4(b"mdia", &[mdhd, hdlr, minf].concat());
    let tkhd = full_box(b"tkhd", 7, &{
        let mut body = vec![0u8; 80];
        body[8..12].copy_from_slice(&1u32.to_be_bytes());
        body[16..20].copy_from_slice(&duration_ms.to_be_bytes());
        body[36..40].copy_from_slice(&0x00010000u32.to_be_bytes());
        body[52..56].copy_from_slice(&0x00010000u32.to_be_bytes());
        body[68..72].copy_from_slice(&0x40000000u32.to_be_bytes());
        body[72..76].copy_from_slice(&(side << 16).to_be_bytes());
        body[76..80].copy_from_slice(&(side << 16).to_be_bytes());
        body
    });
    let trak = box4(b"trak", &[tkhd, mdia].concat());
    let mvhd = full_box(b"mvhd", 0, &{
        let mut body = vec![0u8; 96];
        body[8..12].copy_from_slice(&1000u32.to_be_bytes());
        body[12..16].copy_from_slice(&duration_ms.to_be_bytes());
        body[16..20].copy_from_slice(&0x00010000u32.to_be_bytes());
        body[20..22].copy_from_slice(&0x0100u16.to_be_bytes());
        body[32..36].copy_from_slice(&0x00010000u32.to_be_bytes());
        body[48..52].copy_from_slice(&0x00010000u32.to_be_bytes());
        body[64..68].copy_from_slice(&0x40000000u32.to_be_bytes());
        body[92..96].copy_from_slice(&2u32.to_be_bytes());
        body
    });
    let moov = box4(b"moov", &[mvhd, trak].concat());
    [ftyp, mdat, moov].concat()
}

fn avc_decoder_config(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let profile = sps.get(1).copied().unwrap_or(0x42);
    let compat = sps.get(2).copied().unwrap_or(0);
    let level = sps.get(3).copied().unwrap_or(0x1f);
    let mut body = vec![1, profile, compat, level, 0xff, 0xe1];
    body.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    body.extend_from_slice(sps);
    body.push(1);
    body.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    body.extend_from_slice(pps);
    box4(b"avcC", &body)
}

fn box4(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full_box(kind: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + body.len());
    payload.extend_from_slice(&flags.to_be_bytes());
    payload.extend_from_slice(body);
    box4(kind, &payload)
}

/// The lab's round-record driver: the note's JSON, the attribute (round, square, no sound,
/// `video/mp4`), the minute and 384 px clamp, and a 5 second test pattern whose MP4 boxes
/// are a video-only `avc1` file. No camera and no AAC encoder.
pub fn prove_round_record() -> Result<(), String> {
    let note = crate::VideoNote { duration: 5_000, side: ROUND_SIDE, sound: false };
    let json = serde_json::to_string(&note).map_err(|err| err.to_string())?;
    if serde_json::from_str::<crate::VideoNote>(&json).map_err(|err| err.to_string())? != note {
        return Err("a video note did not read back".into());
    }
    let older: crate::VideoNote = serde_json::from_str(r#"{"duration":1000,"side":384}"#).map_err(|err| err.to_string())?;
    if older.sound || older.duration != 1000 {
        return Err("a video note without sound was not picture only".into());
    }
    let (side, duration) = clamp_round(1920, 120_000);
    if side != ROUND_SIDE || duration != ROUND_LIMIT_MS {
        return Err(format!("a long note was not clamped to {ROUND_SIDE}px and one minute"));
    }
    let (odd, _) = clamp_round(101, 1_000);
    if odd != 100 {
        return Err("an odd side was not made even".into());
    }
    let wire = round_wire(&note);
    if !wire.round_message || !wire.nosound || wire.mime != "video/mp4" || wire.side != ROUND_SIDE as i32 {
        return Err(format!("the attribute is not a silent round note: {wire:?}"));
    }
    if (wire.duration_secs - 5.0).abs() > 0.001 {
        return Err("five seconds was not five seconds".into());
    }
    let sounding = round_wire(&crate::VideoNote { sound: true, ..note.clone() });
    if sounding.nosound {
        return Err("a note with sound was marked silent".into());
    }
    let sps = vec![0x67, 0x42, 0x00, 0x1f];
    let pps = vec![0x68, 0xce, 0x38, 0x80];
    let mut frames = Vec::new();
    for i in 0..75u8 {
        let mut frame = Vec::new();
        if i == 0 {
            frame.extend_from_slice(&[0, 0, 0, 1]);
            frame.extend_from_slice(&sps);
            frame.extend_from_slice(&[0, 0, 0, 1]);
            frame.extend_from_slice(&pps);
        }
        frame.extend_from_slice(&[0, 0, 0, 1]);
        let kind = if i % 15 == 0 { 0x65 } else { 0x41 };
        frame.extend_from_slice(&[kind, i, 0x11]);
        frames.push(frame);
    }
    let file = round_mp4(ROUND_SIDE, &frames, 66);
    if file.samples != 75 || file.side != ROUND_SIDE || file.duration_ms != 75 * 66 {
        return Err(format!("the test pattern is {} samples, {} ms, side {}", file.samples, file.duration_ms, file.side));
    }
    if file.duration_ms > ROUND_LIMIT_MS {
        return Err("the test pattern is longer than a minute".into());
    }
    let kinds = top_boxes(&file.bytes)?;
    if kinds != [b"ftyp", b"mdat", b"moov"] {
        return Err("the file is not ftyp, mdat, moov".into());
    }
    if !has_box(&file.bytes, b"avc1") || !has_box(&file.bytes, b"avcC") {
        return Err("the file has no avc1 sample entry".into());
    }
    if file.bytes.windows(4).any(|mark| mark == b"soun") {
        return Err("the file has a sound track".into());
    }
    if !file.bytes.windows(sps.len()).any(|mark| mark == sps) {
        return Err("avcC has no sequence parameter set".into());
    }
    let start = RecordRequest::Start {
        conversation: crate::Key { network: crate::Network::Telegram, account: "1".into(), id: "7".into() },
        input: None,
        video: true,
    };
    let asked = serde_json::to_string(&start).map_err(|err| err.to_string())?;
    if !asked.contains(r#""video":true"#) {
        return Err("a video recording was not asked for".into());
    }
    if serde_json::from_str::<RecordRequest>(&asked).map_err(|err| err.to_string())? != start {
        return Err("a video recording did not read back".into());
    }
    let frame = RecEvent::Frame { path: "preview.jpg".into() };
    let told = serde_json::to_string(&frame).map_err(|err| err.to_string())?;
    if !told.contains(r#""recorder":"frame""#) {
        return Err("a preview was not a recorder frame".into());
    }
    Ok(())
}

fn top_boxes(bytes: &[u8]) -> Result<Vec<[u8; 4]>, String> {
    let mut i = 0;
    let mut kinds = Vec::new();
    while i + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[i..i + 4].try_into().map_err(|_| "box size")?) as usize;
        if size < 8 || i + size > bytes.len() {
            return Err(format!("a box at {i} does not fit"));
        }
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&bytes[i + 4..i + 8]);
        kinds.push(kind);
        i += size;
    }
    if i != bytes.len() {
        return Err("boxes do not fill the file".into());
    }
    Ok(kinds)
}

fn has_box(bytes: &[u8], kind: &[u8; 4]) -> bool {
    bytes.windows(8).any(|window| {
        let size = u32::from_be_bytes([window[0], window[1], window[2], window[3]]);
        size >= 8 && &window[4..8] == kind
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_back_as_written() {
        let shown = Recording {
            conversation: Key { network: crate::Network::Telegram, account: "1".into(), id: "7002".into() },
            since: 10_000,
            device: Some("Blue Yeti".into()),
            notice: None,
            video: false,
            side: 0,
            frame: None,
            state: RecordState::Recorded { path: "/media/telegram/1/voice.opus".into(), duration: 12_000, waveform: vec![31, 0] },
        };
        let json = serde_json::to_string(&shown).unwrap();
        assert!(json.contains(r#""state":"recorded""#), "{json}");
        assert_eq!(serde_json::from_str::<Recording>(&json).unwrap(), shown);
        let failed = Recording {
            conversation: shown.conversation.clone(),
            since: 10_000,
            device: None,
            notice: None,
            video: false,
            side: 0,
            frame: None,
            state: RecordState::Failed { detail: "No microphone.".into() },
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains(r#""state":"failed""#), "{json}");
        assert_eq!(serde_json::from_str::<Recording>(&json).unwrap(), failed);
    }

    #[test]
    fn a_round_recording_is_a_silent_square_mp4() {
        prove_round_record().unwrap();
    }
}
