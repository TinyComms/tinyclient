//! Screen share: ReplayKit's in-app capture (`RPScreenRecorder.startCapture`), which records
//! this app's own screen after the system asks the user once.
//!
//! Sharing other apps or the home screen needs a Broadcast Upload Extension: a separate process
//! whose frames reach the app through an App Group (see docs/mobile-platform.md). Not built yet.

use block2::RcBlock;
use mediad::platform::FrameSink;
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::NSError;
use objc2_replay_kit::{RPSampleBufferType, RPScreenRecorder};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// A share is running.
static SHARING: AtomicBool = AtomicBool::new(false);
/// When the last frame was handed on, for a rate mediad can use (it sends one a second).
static LAST_US: AtomicU64 = AtomicU64::new(0);
/// Frames handed on at most this often.
const EVERY_US: u64 = 200_000;
/// Video samples ReplayKit delivered, and the last one's pixel format, for the log.
static SAMPLES: AtomicU64 = AtomicU64::new(0);
static FORMAT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Starts sharing and returns once frames come or the user (or the system) refuses.
pub(super) fn start(sink: FrameSink) -> Result<(), String> {
    stop();
    LAST_US.store(0, Ordering::Relaxed);
    let (sender, started) = mpsc::channel::<Result<(), String>>();
    let handler = RcBlock::new(move |sample: NonNull<CMSampleBuffer>, kind: RPSampleBufferType, error: *mut NSError| {
        if !error.is_null() || kind != RPSampleBufferType::Video {
            return;
        }
        SAMPLES.fetch_add(1, Ordering::Relaxed);
        let now = super::pixels::now_us();
        if now.saturating_sub(LAST_US.load(Ordering::Relaxed)) < EVERY_US {
            return;
        }
        LAST_US.store(now, Ordering::Relaxed);
        // SAFETY: ReplayKit hands a valid sample buffer for the call.
        let Some(image) = (unsafe { sample.as_ref().image_buffer() }) else {
            return;
        };
        // SAFETY: reading the buffer's format.
        FORMAT.store(unsafe { objc2_core_video::CVPixelBufferGetPixelFormatType(&image) }, Ordering::Relaxed);
        if let Some(frame) = super::pixels::frame_of(&image) {
            sink(frame);
        }
    });
    let done = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: ReplayKit's error, valid for the call, or null.
        let answer = match unsafe { error.as_ref() } {
            None => Ok(()),
            Some(error) => Err(format!("The screen can't be shared ({}).", error.localizedDescription())),
        };
        let _ = sender.send(answer);
    });
    let blocks = super::Shared((handler, done));
    super::on_main(move |_| {
        let (handler, done) = blocks.get();
        // SAFETY: the shared recorder, started on the main thread; ReplayKit copies the blocks.
        unsafe {
            let recorder = RPScreenRecorder::sharedRecorder();
            if !recorder.isAvailable() {
                return false;
            }
            recorder.setMicrophoneEnabled(false);
            recorder.startCaptureWithHandler_completionHandler(Some(handler), Some(done));
            true
        }
    })
    .then_some(())
    .ok_or_else(|| "Screen sharing isn't available on this device right now.".to_owned())?;
    // The system asks the user first; a minute is long enough for anyone to answer.
    let answer = started.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| Err("The screen share did not start.".to_owned()));
    if answer.is_ok() {
        SHARING.store(true, Ordering::Relaxed);
        log::info!("tinyclient: screen share running");
    }
    answer
}

/// Stops sharing, if it runs.
pub(super) fn stop() {
    if !SHARING.swap(false, Ordering::Relaxed) {
        return;
    }
    let format = FORMAT.swap(0, Ordering::Relaxed).to_be_bytes();
    log::info!(
        "tinyclient: screen share delivered {} video samples (format {:?})",
        SAMPLES.swap(0, Ordering::Relaxed),
        String::from_utf8_lossy(&format)
    );
    super::on_main(|_| {
        // SAFETY: the shared recorder, on the main thread.
        unsafe { RPScreenRecorder::sharedRecorder().stopCaptureWithHandler(None) };
    });
    log::info!("tinyclient: screen share stopped");
}
