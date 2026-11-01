//! My own camera on the call screen: the camera runs in this process, so the newest frame it
//! hands mediad is also kept here and written as a small JPEG about ten times a second, which the
//! call screen's tile shows (`gui::call::phone::hooks::self_frame`). The front camera's picture
//! is mirrored, as a mirror would show it; what is sent is not.

use mediad::platform::{FrameSink, VideoFrame};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

/// The newest frame, not yet written; `None` once written.
static NEWEST: Mutex<Option<VideoFrame>> = Mutex::new(None);
/// The JPEG the tile shows now, while the camera runs.
static SHOWN: Mutex<Option<PathBuf>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static MIRROR: AtomicBool = AtomicBool::new(true);
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The longest side of the tile's picture.
const EDGE: u32 = 480;

/// `sink`, also keeping each frame for the tile.
pub fn tee(sink: FrameSink, front: bool) -> FrameSink {
    MIRROR.store(front, Ordering::Relaxed);
    RUNNING.store(true, Ordering::Relaxed);
    start_writer();
    Arc::new(move |frame: VideoFrame| {
        *NEWEST.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame.clone());
        sink(frame);
    })
}

pub fn mirror(front: bool) {
    MIRROR.store(front, Ordering::Relaxed);
}

/// The camera stopped: the tile goes back to my face.
pub fn stop() {
    RUNNING.store(false, Ordering::Relaxed);
    NEWEST.lock().unwrap_or_else(|p| p.into_inner()).take();
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).take();
}

/// For the call screen.
pub fn shown() -> Option<PathBuf> {
    if !RUNNING.load(Ordering::Relaxed) {
        return None;
    }
    SHOWN.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn dir() -> PathBuf {
    protocol::support_dir().join("pictures")
}

fn start_writer() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = std::fs::create_dir_all(dir());
        let _ = std::thread::Builder::new().name("tiny-selfie".into()).spawn(|| loop {
            std::thread::sleep(Duration::from_millis(100));
            let Some(frame) = NEWEST.lock().unwrap_or_else(|p| p.into_inner()).take() else {
                continue;
            };
            if !RUNNING.load(Ordering::Relaxed) {
                continue;
            }
            match write(&frame) {
                Ok(path) => *SHOWN.lock().unwrap_or_else(|p| p.into_inner()) = Some(path),
                Err(err) => log::warn!("tinyclient: my camera's picture was not written: {err}"),
            }
        });
    });
}

fn write(frame: &VideoFrame) -> std::io::Result<PathBuf> {
    let (width, height) = mediad::picture::fit(frame.width, frame.height, EDGE, EDGE);
    let planes = mediad::picture::convert::planes(frame, width, height);
    let mut rgb = mediad::picture::rgb(&planes);
    if MIRROR.load(Ordering::Relaxed) {
        let row = width as usize * 3;
        for line in rgb.chunks_mut(row) {
            let pixels = width as usize;
            for x in 0..pixels / 2 {
                let (a, b) = (x * 3, (pixels - 1 - x) * 3);
                for c in 0..3 {
                    line.swap(a + c, b + c);
                }
            }
        }
    }
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let path = dir().join(format!("self-{generation}.jpg"));
    let file = std::fs::File::create(&path)?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 75);
    encoder.encode(&rgb, width, height, image::ExtendedColorType::Rgb8).map_err(std::io::Error::other)?;
    // The tile may still be drawing the one before; older ones go.
    if generation >= 4 {
        let _ = std::fs::remove_file(dir().join(format!("self-{}.jpg", generation - 4)));
    }
    Ok(path)
}
