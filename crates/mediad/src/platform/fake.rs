//! A platform with no hardware, for host tests and for trying the plumbing: a camera and a
//! screen that paint moving stripes in NV12, and a passthrough "codec" whose bitstream is the
//! frame itself behind an IDR-looking header. It proves the wiring, not H.264: its output is
//! not decodable by anything else.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// What the fake was asked to do, for a test to look at.
#[derive(Default)]
pub struct Log {
    pub permissions: Mutex<Vec<Permission>>,
    pub audio_sessions: AtomicUsize,
    pub audio_active: AtomicBool,
    pub speaker: AtomicBool,
    pub cameras: Mutex<Vec<CameraRequest>>,
    pub encoders: Mutex<Vec<EncoderConfig>>,
    pub decoders: AtomicUsize,
}

#[derive(Default)]
pub struct FakePlatform {
    pub log: Arc<Log>,
    /// Refuses every permission when set.
    pub deny: AtomicBool,
    camera: Mutex<Option<Arc<AtomicBool>>>,
    screen: Mutex<Option<Arc<AtomicBool>>>,
}

/// One NV12 frame of stripes that move with `n`.
pub fn stripes(width: u32, height: u32, n: u64) -> VideoFrame {
    let (w, h) = (width as usize, height as usize);
    let mut data = vec![128u8; VideoFrame::expected_len(width, height, PixelFormat::Nv12)];
    for row in 0..h {
        for col in 0..w {
            data[row * w + col] = if (col + n as usize) / 8 % 2 == 0 { 40 } else { 220 };
        }
    }
    VideoFrame { width, height, format: PixelFormat::Nv12, data, timestamp_us: n * 66_000 }
}

/// Paints into `sink` at `fps` until `stop`.
fn paint(width: u32, height: u32, fps: u32, sink: FrameSink, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("fake-capture".into()).spawn(move || {
        let mut n = 0;
        while !stop.load(Ordering::Relaxed) {
            sink(stripes(width, height, n));
            n += 1;
            std::thread::sleep(Duration::from_millis(1000 / u64::from(fps.max(1))));
        }
    });
}

fn replace(slot: &Mutex<Option<Arc<AtomicBool>>>) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(old) = slot.lock().unwrap().replace(stop.clone()) {
        old.store(true, Ordering::Relaxed);
    }
    stop
}

fn halt(slot: &Mutex<Option<Arc<AtomicBool>>>) {
    if let Some(old) = slot.lock().unwrap().take() {
        old.store(true, Ordering::Relaxed);
    }
}

impl FakePlatform {
    /// True while the fake camera paints.
    pub fn camera_on(&self) -> bool {
        self.camera.lock().unwrap().is_some()
    }

    /// True while the fake screen paints.
    pub fn screen_on(&self) -> bool {
        self.screen.lock().unwrap().is_some()
    }
}

impl Platform for FakePlatform {
    fn request_permission(&self, permission: Permission) -> Result<(), String> {
        self.log.permissions.lock().unwrap().push(permission);
        if self.deny.load(Ordering::Relaxed) {
            return Err("Denied by the test.".to_owned());
        }
        Ok(())
    }

    fn start_audio_session(&self) -> Result<(), String> {
        self.log.audio_sessions.fetch_add(1, Ordering::Relaxed);
        self.log.audio_active.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn stop_audio_session(&self) {
        self.log.audio_active.store(false, Ordering::Relaxed);
    }

    fn set_speaker(&self, on: bool) -> Result<(), String> {
        self.log.speaker.store(on, Ordering::Relaxed);
        Ok(())
    }

    fn start_camera(&self, request: CameraRequest, sink: FrameSink) -> Result<(), String> {
        self.log.cameras.lock().unwrap().push(request);
        let stop = replace(&self.camera);
        paint(request.width, request.height, request.fps, sink, stop);
        Ok(())
    }

    fn stop_camera(&self) {
        halt(&self.camera);
    }

    fn start_screen_share(&self, sink: FrameSink) -> Result<(), String> {
        let stop = replace(&self.screen);
        paint(1920, 1080, 15, sink, stop);
        Ok(())
    }

    fn stop_screen_share(&self) {
        halt(&self.screen);
    }

    fn h264_encoder(&self, config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String> {
        self.log.encoders.lock().unwrap().push(config);
        Ok(Box::new(Passthrough))
    }

    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String> {
        self.log.decoders.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Passthrough))
    }
}

/// The passthrough codec: `00 00 00 01 65`, width and height (u32, big endian), the NV12.
pub struct Passthrough;

const HEAD: [u8; 5] = [0, 0, 0, 1, 0x65];

impl H264Encoder for Passthrough {
    fn encode(&mut self, frame: &VideoFrame, _keyframe: bool) -> Result<Vec<u8>, String> {
        if frame.format != PixelFormat::Nv12 || !frame.is_whole() {
            return Err("The fake encoder takes whole NV12 frames.".to_owned());
        }
        let mut out = HEAD.to_vec();
        out.extend_from_slice(&frame.width.to_be_bytes());
        out.extend_from_slice(&frame.height.to_be_bytes());
        out.extend_from_slice(&frame.data);
        Ok(out)
    }
}

impl H264Decoder for Passthrough {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String> {
        let Some(rest) = annex_b.strip_prefix(&HEAD[..]) else {
            return Ok(None);
        };
        let number = |at: usize| rest.get(at..at + 4).map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        let (Some(width), Some(height)) = (number(0), number(4)) else {
            return Ok(None);
        };
        let frame = VideoFrame { width, height, format: PixelFormat::Nv12, data: rest[8..].to_vec(), timestamp_us: 0 };
        Ok(frame.is_whole().then_some(frame))
    }
}
