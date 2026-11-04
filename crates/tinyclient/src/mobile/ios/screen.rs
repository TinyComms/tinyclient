//! Screen share.
//!
//! A device records the whole screen through a Broadcast Upload Extension
//! (`mobile/ios/Broadcast`). The extension is another process, so its frames land in the app
//! group's mailbox (`mediad::broadcast`) and a thread here copies them to the sink. The system
//! picker is `RPSystemBroadcastPickerView`, which objc2 does not bind, so it is messaged the
//! same way WebKit is.
//!
//! The simulator has no system broadcast. It keeps ReplayKit's in-app capture, which records
//! this app only.

#[cfg(target_abi = "sim")]
use block2::RcBlock;
use mediad::platform::FrameSink;
#[cfg(not(target_abi = "sim"))]
use objc2::rc::{Allocated, Retained};
#[cfg(not(target_abi = "sim"))]
use objc2::runtime::{AnyClass, AnyObject, Bool};
#[cfg(not(target_abi = "sim"))]
use objc2::{msg_send, Message};
#[cfg(not(target_abi = "sim"))]
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
#[cfg(target_abi = "sim")]
use objc2_core_media::CMSampleBuffer;
#[cfg(target_abi = "sim")]
use objc2_foundation::NSError;
#[cfg(not(target_abi = "sim"))]
use objc2_foundation::{NSFileManager, NSString};
#[cfg(target_abi = "sim")]
use objc2_replay_kit::{RPSampleBufferType, RPScreenRecorder};
#[cfg(not(target_abi = "sim"))]
use objc2_ui_kit::{UIButton, UIControlEvents, UIView};
#[cfg(target_abi = "sim")]
use std::ptr::NonNull;
#[cfg(target_abi = "sim")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(target_abi = "sim"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[cfg(not(target_abi = "sim"))]
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(target_abi = "sim")]
static SHARING: AtomicBool = AtomicBool::new(false);
#[cfg(target_abi = "sim")]
static LAST_US: AtomicU64 = AtomicU64::new(0);
#[cfg(target_abi = "sim")]
const EVERY_US: u64 = 200_000;
#[cfg(target_abi = "sim")]
static SAMPLES: AtomicU64 = AtomicU64::new(0);
#[cfg(target_abi = "sim")]
static FORMAT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The extension's bundle id, the `preferredExtension` the picker is told to open.
#[cfg(not(target_abi = "sim"))]
const EXTENSION: &str = "dev.tinyclient.app.broadcast";

#[cfg(not(target_abi = "sim"))]
struct Running {
    map: Arc<mediad::broadcast::Map>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_abi = "sim"))]
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Starts sharing and returns once frames come or the user (or the system) refuses.
pub(super) fn start(sink: FrameSink) -> Result<(), String> {
    #[cfg(target_abi = "sim")]
    {
        start_replaykit(sink)
    }
    #[cfg(not(target_abi = "sim"))]
    {
        start_broadcast(sink)
    }
}

/// Stops sharing, if it runs.
pub(super) fn stop() {
    #[cfg(target_abi = "sim")]
    stop_replaykit();
    #[cfg(not(target_abi = "sim"))]
    stop_broadcast();
}

#[cfg(target_abi = "sim")]
fn start_replaykit(sink: FrameSink) -> Result<(), String> {
    stop_replaykit();
    LAST_US.store(0, Ordering::Relaxed);
    let (sender, started) = mpsc::channel::<Result<(), String>>();
    let handler = RcBlock::new(
        move |sample: NonNull<CMSampleBuffer>, kind: RPSampleBufferType, error: *mut NSError| {
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
            FORMAT.store(
                unsafe { objc2_core_video::CVPixelBufferGetPixelFormatType(&image) },
                Ordering::Relaxed,
            );
            if let Some(frame) = super::pixels::frame_of(&image) {
                sink(frame);
            }
        },
    );
    let done = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: ReplayKit's error, valid for the call, or null.
        let answer = match unsafe { error.as_ref() } {
            None => Ok(()),
            Some(error) => Err(format!(
                "The screen can't be shared ({}).",
                error.localizedDescription()
            )),
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
    let answer = started
        .recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| Err("The screen share did not start.".to_owned()));
    if answer.is_ok() {
        SHARING.store(true, Ordering::Relaxed);
        log::info!("tinyclient: screen share running");
    }
    answer
}

#[cfg(target_abi = "sim")]
fn stop_replaykit() {
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

#[cfg(not(target_abi = "sim"))]
fn start_broadcast(sink: FrameSink) -> Result<(), String> {
    stop_broadcast();
    let path = container_path()?;
    let map = mediad::broadcast::map_file(&path)
        .map_err(|err| format!("The screen share could not be prepared ({err})."))?;
    mediad::broadcast::prepare(&map);
    let map = Arc::new(map);
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let reading = map.clone();
    let reader = std::thread::Builder::new()
        .name("ios-broadcast".into())
        .spawn(move || read_loop(reading, flag, sink, tx))
        .map_err(|err| err.to_string())?;
    *RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Running {
        map,
        stop,
        reader: Some(reader),
    });
    if let Err(words) = super::on_main(show_picker) {
        stop_broadcast();
        return Err(words);
    }
    // The system sheet asks which app to broadcast. A minute is long enough to answer.
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => {
            log::info!("tinyclient: screen broadcast running");
            Ok(())
        }
        Err(_) => {
            stop_broadcast();
            Err("The screen share did not start.".to_owned())
        }
    }
}

#[cfg(not(target_abi = "sim"))]
fn stop_broadcast() {
    let running = RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(mut running) = running else {
        return;
    };
    running.stop.store(true, Ordering::Relaxed);
    mediad::broadcast::set_state(&running.map, mediad::broadcast::STATE_STOP);
    if let Some(reader) = running.reader.take() {
        let _ = reader.join();
    }
    log::info!("tinyclient: screen broadcast stopped");
}

#[cfg(not(target_abi = "sim"))]
fn read_loop(
    map: Arc<mediad::broadcast::Map>,
    stop: Arc<AtomicBool>,
    sink: FrameSink,
    started: mpsc::Sender<()>,
) {
    let mut last = 0u32;
    let mut told = false;
    while !stop.load(Ordering::Relaxed) {
        if let Some(mail) = mediad::broadcast::read(&map) {
            if mail.seq != last {
                last = mail.seq;
                if let Some(frame) = video_of(mail) {
                    sink(frame);
                    if !told {
                        told = true;
                        let _ = started.send(());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(target_abi = "sim"))]
fn video_of(mail: mediad::broadcast::MailFrame) -> Option<mediad::platform::VideoFrame> {
    use mediad::platform::{PixelFormat, VideoFrame};
    let format = match mail.format {
        mediad::broadcast::FORMAT_BGRA => PixelFormat::Bgra,
        mediad::broadcast::FORMAT_NV12 => PixelFormat::Nv12,
        _ => return None,
    };
    if mail.data.len() != VideoFrame::expected_len(mail.width, mail.height, format) {
        return None;
    }
    Some(VideoFrame {
        width: mail.width,
        height: mail.height,
        format,
        data: mail.data,
        timestamp_us: mail.timestamp_us,
    })
}

#[cfg(not(target_abi = "sim"))]
fn container_path() -> Result<std::path::PathBuf, String> {
    let url = NSFileManager::defaultManager().containerURLForSecurityApplicationGroupIdentifier(
        &NSString::from_str(mediad::broadcast::GROUP),
    );
    let Some(url) = url else {
        return Err("This build can't share the whole screen. The screen recording group is not in the signature.".to_owned());
    };
    let Some(path) = url.path() else {
        return Err("This build can't share the whole screen. The screen recording group is not in the signature.".to_owned());
    };
    Ok(std::path::PathBuf::from(path.to_string()).join(mediad::broadcast::FILE_NAME))
}

/// Shows the system's broadcast picker and taps its button, which is the only way to open it.
#[cfg(not(target_abi = "sim"))]
fn show_picker(mtm: objc2::MainThreadMarker) -> Result<(), String> {
    let Some(class) = AnyClass::get(c"RPSystemBroadcastPickerView") else {
        return Err("This iOS version has no screen broadcast picker.".to_owned());
    };
    let Some(window) = super::auth::key_window(mtm) else {
        return Err("The screen broadcast control could not be shown.".to_owned());
    };
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(60.0, 60.0));
    // SAFETY: RPSystemBroadcastPickerView is a UIView. The selectors are the ones the header
    // declares. The view is retained until it is removed below.
    let view = unsafe {
        let alloc: Allocated<AnyObject> = msg_send![class, alloc];
        let picker: Retained<AnyObject> = msg_send![alloc, initWithFrame: frame];
        let extension = NSString::from_str(EXTENSION);
        let _: () = msg_send![&*picker, setPreferredExtension: &*extension];
        let _: () = msg_send![&*picker, setShowsMicrophoneButton: Bool::NO];
        picker
            .downcast::<UIView>()
            .map_err(|_| "The screen broadcast control could not be shown.".to_owned())?
    };
    window.addSubview(&view);
    view.layoutIfNeeded();
    let Some(button) = find_button(&view) else {
        view.removeFromSuperview();
        return Err("The screen broadcast control could not be shown.".to_owned());
    };
    button.sendActionsForControlEvents(UIControlEvents::TouchUpInside);
    view.removeFromSuperview();
    Ok(())
}

#[cfg(not(target_abi = "sim"))]
fn find_button(view: &UIView) -> Option<Retained<UIButton>> {
    if let Some(button) = view.downcast_ref::<UIButton>() {
        return Some(button.retain());
    }
    for child in view.subviews().iter() {
        if let Some(found) = find_button(child) {
            return Some(found);
        }
    }
    None
}
