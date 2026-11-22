//! `mediad --rec-video <path>`: one round video message, camera and a square H.264 picture,
//! into one video-only MP4, until chatd closes stdin, a minute passes, or the camera fails.
//!
//! There is no AAC encoder in this build, and Opus inside an MP4 is not what Telegram plays.
//! The file has no sound track. The preview is a JPEG beside the recording. A camera that
//! will not open is said as a failure, not filled with a test pattern.

use crate::out;
use crate::picture::{self, Rgba};
use protocol::{read_frame, RecEvent, ROUND_FPS, ROUND_LIMIT_MS, ROUND_SIDE};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(1000 / ROUND_FPS as u64);
const FRAME_MS: u32 = 1000 / ROUND_FPS;
const PREVIEW_EVERY: Duration = Duration::from_millis(200);
const NO_SOUND: &str = "Sound is not in this video message. This build has no AAC encoder.";

fn stopper() -> Receiver<()> {
    let (tell, told) = channel();
    let _ = out::spawn(std::thread::Builder::new().name("stopper".to_owned()), move || {
        let mut stdin = out::stdin();
        loop {
            match read_frame::<protocol::PlayerRequest>(&mut stdin) {
                Ok(_) => continue,
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => break,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    break;
                }
            }
        }
        let _ = tell.send(());
    });
    told
}

fn fail(detail: impl Into<String>) -> ! {
    out::write(&RecEvent::Failed { detail: detail.into() });
    out::exit(2)
}

struct Shot {
    annex: Vec<u8>,
    preview: Vec<u8>,
}

fn crop_square(src: &Rgba) -> Result<Rgba, String> {
    if src.width < 2 || src.height < 2 || src.pixels.len() < (src.width as usize * src.height as usize * 4) {
        return Err("The camera produced no picture.".into());
    }
    let side = src.width.min(src.height);
    let x0 = (src.width - side) / 2;
    let y0 = (src.height - side) / 2;
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for row in 0..side {
        let start = (((y0 + row) * src.width + x0) * 4) as usize;
        let end = start + (side * 4) as usize;
        pixels.extend_from_slice(&src.pixels[start..end]);
    }
    Ok(Rgba { width: side, height: side, pixels, bgra: src.bgra })
}

fn write_preview(rgb: &[u8], side: u32, path: &Path) -> Option<String> {
    let image = image::RgbImage::from_raw(side, side, rgb.to_vec())?;
    image.save_with_format(path, image::ImageFormat::Jpeg).ok()?;
    Some(path.display().to_string())
}

fn finish(path: &Path, frames: &[Vec<u8>]) {
    if frames.is_empty() {
        let _ = std::fs::remove_file(path);
        fail("The camera produced no picture.");
    }
    let file = protocol::round_mp4(ROUND_SIDE, frames, FRAME_MS);
    if std::fs::write(path, &file.bytes).is_err() {
        fail("The recording could not be written.");
    }
    out::write(&RecEvent::Done { duration: file.duration_ms.min(ROUND_LIMIT_MS), waveform: Vec::new() });
}

fn record(path: PathBuf, told: Receiver<()>, name: String, mut grab: impl FnMut(Duration, bool) -> Result<Option<Shot>, String>) {
    out::write(&RecEvent::Started { device: name, notice: Some(NO_SOUND.to_owned()) });
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut generation = 0u64;
    let mut preview_at = Instant::now();
    let mut shown: Option<PathBuf> = None;
    let started = Instant::now();
    let mut index = 0u32;
    loop {
        if told.try_recv().is_ok() || started.elapsed() >= Duration::from_millis(u64::from(ROUND_LIMIT_MS)) {
            break;
        }
        let at = started.elapsed();
        let key = index % ROUND_FPS == 0;
        match grab(at, key) {
            Ok(Some(shot)) => {
                if !shot.annex.is_empty() {
                    frames.push(shot.annex);
                    index += 1;
                }
                if preview_at.elapsed() >= PREVIEW_EVERY && shot.preview.len() == (ROUND_SIDE * ROUND_SIDE * 3) as usize {
                    generation += 1;
                    let jpeg = path.with_file_name(format!("round-preview-{generation}.jpg"));
                    if let Some(told_path) = write_preview(&shot.preview, ROUND_SIDE, &jpeg) {
                        if let Some(old) = shown.replace(jpeg) {
                            let _ = std::fs::remove_file(old);
                        }
                        out::write(&RecEvent::Frame { path: told_path });
                        preview_at = Instant::now();
                    }
                }
            }
            Ok(None) => {}
            Err(detail) => fail(detail),
        }
        match told.recv_timeout(FRAME) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    finish(&path, &frames);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn run(path: PathBuf, input: Option<String>) -> io::Result<()> {
    let told = stopper();
    let mut cam = match picture::RoundCam::open(input.as_deref()) {
        Ok(cam) => cam,
        Err(detail) => fail(detail),
    };
    let mut encoder = match picture::RoundEncoder::open() {
        Ok(encoder) => encoder,
        Err(detail) => fail(detail),
    };
    let name = cam.name().to_owned();
    record(path, told, name, |at, key| {
        let rgba = cam.grab()?;
        let square = crop_square(&rgba)?;
        let planes = picture::i420(&square.pixels, square.width, square.height, ROUND_SIDE, ROUND_SIDE, square.bgra);
        let encoded = encoder.encode(&planes, at, key)?;
        let preview = picture::rgb(&planes);
        Ok(Some(Shot { annex: encoded.bytes, preview }))
    });
    Ok(())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn run(path: PathBuf, _input: Option<String>) -> io::Result<()> {
    use crate::platform::{self, CameraRequest, EncoderConfig, Facing, H264Encoder, Permission};
    use std::sync::{Arc, Mutex};

    let told = stopper();
    let platform = match platform::required() {
        Ok(platform) => platform,
        Err(detail) => fail(detail),
    };
    if let Err(detail) = platform.request_permission(Permission::Camera) {
        fail(detail);
    }
    let newest = Arc::new(Mutex::new(None));
    let sink_slot = newest.clone();
    let sink: platform::FrameSink = Arc::new(move |frame| {
        if frame.is_whole() {
            *sink_slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(frame);
        }
    });
    let request = CameraRequest { facing: Facing::Front, width: ROUND_SIDE, height: ROUND_SIDE, fps: ROUND_FPS };
    if let Err(detail) = platform.start_camera(request, sink) {
        fail(detail);
    }
    let mut encoder = match platform.h264_encoder(EncoderConfig {
        width: ROUND_SIDE,
        height: ROUND_SIDE,
        bitrate_bps: 400_000,
        fps: ROUND_FPS,
        screen: false,
    }) {
        Ok(encoder) => encoder,
        Err(detail) => {
            platform.stop_camera();
            fail(detail);
        }
    };
    let platform_stop = platform.clone();
    record(path, told, "Camera".to_owned(), move |_at, key| {
        let frame = newest.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let Some(frame) = frame else {
            return Ok(None);
        };
        let planes = picture::convert::planes(&frame, ROUND_SIDE, ROUND_SIDE);
        let packed = picture::convert::nv12(&planes);
        let annex = encoder.encode(&packed, key)?;
        let preview = picture::rgb(&planes);
        Ok(Some(Shot { annex, preview }))
    });
    platform_stop.stop_camera();
    Ok(())
}
