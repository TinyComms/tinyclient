//! The demo build's call (`gui::shot_demo`): my camera through the real picture path and back.
//! mediad's own phone pictures (`mediad::picture::phone::Pictures`) pull the platform camera's
//! newest frame 15 times a second, fit it to the 640×360 and 320×180 layers and encode both with
//! MediaCodec, exactly as a Tiny call does; the 640×360 layer is then decoded with MediaCodec and
//! written as the JPEG the call screen shows as the other side's picture. Proof without a second
//! device that camera, encoder and decoder work together on the phone.

use mediad::picture::phone::Pictures;
use protocol::{CallPicture, PictureKind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static RUNNING: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(1);
static PICTURE: Mutex<Option<CallPicture>> = Mutex::new(None);

pub fn register() {
    crate::gui::shot_demo::route_loopback(camera, picture, call);
}

/// The call's audio side, as mediad's call loop does it: the microphone asked for (blocking
/// until answered), then the audio session (communication mode, the route, the foreground
/// service and its notification) held until the call ends.
fn call(on: bool) {
    static SESSION: Mutex<Option<mediad::platform::AudioSession>> = Mutex::new(None);
    if !on {
        RUNNING.store(false, Ordering::Relaxed);
        std::thread::spawn(|| drop(SESSION.lock().unwrap_or_else(|p| p.into_inner()).take()));
        return;
    }
    std::thread::spawn(|| {
        let Some(platform) = mediad::platform::get() else {
            return;
        };
        match platform.request_permission(mediad::platform::Permission::Microphone) {
            Ok(()) => {
                log::info!("tinyclient: demo call: the microphone is allowed; starting the audio session");
                *SESSION.lock().unwrap_or_else(|p| p.into_inner()) = Some(mediad::platform::call_audio());
            }
            Err(words) => log::warn!("tinyclient: demo call: {words}"),
        }
    });
}

fn picture() -> Option<CallPicture> {
    PICTURE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn camera(on: bool) {
    if !on {
        RUNNING.store(false, Ordering::Relaxed);
        return;
    }
    if RUNNING.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new().name("tiny-loopback".into()).spawn(run);
}

fn run() {
    log::info!("tinyclient: camera loopback started");
    let mut pictures = Pictures::default();
    let (mut sent, mut bytes_high, mut bytes_low, mut decoded) = (0u64, 0usize, 0usize, 0u64);
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut tick = 0u64;
    while RUNNING.load(Ordering::Relaxed) {
        let at = Instant::now();
        // An IDR every two seconds, as a call does for a new watcher.
        let key = tick % 30 == 0;
        tick += 1;
        match pictures.camera(key) {
            Ok((high, low)) => {
                sent += 1;
                bytes_high += high.len();
                bytes_low += low.len();
                let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
                if let Some((path, width, height)) = pictures.decode(&high, generation) {
                    decoded += 1;
                    let old = PICTURE.lock().unwrap_or_else(|p| p.into_inner()).replace(CallPicture {
                        path: path.to_string_lossy().into_owned(),
                        width,
                        height,
                        generation,
                        user_id: crate::gui::shot_demo::LOOPBACK_ID.into(),
                        kind: PictureKind::Camera,
                    });
                    // The screen may still draw the one before; older ones go.
                    if let Some(old) = old.filter(|old| old.generation + 4 < generation) {
                        let _ = std::fs::remove_file(old.path);
                    }
                }
            }
            Err(words) => {
                if tick % 15 == 1 {
                    log::info!("tinyclient: camera loopback waiting: {words}");
                }
                // "Later" is a camera warming up or an encoder holding a frame; anything else
                // (refused, no camera, no codec) would only fail again.
                if words != mediad::picture::CODEC_LATER {
                    log::warn!("tinyclient: camera loopback stopped: {words}");
                    break;
                }
            }
        }
        if last_report.elapsed() >= Duration::from_secs(2) {
            last_report = Instant::now();
            log::info!(
                "tinyclient: camera loopback {:.0}s: {sent} frames encoded (640x360 {} KB, 320x180 {} KB), {decoded} decoded back",
                started.elapsed().as_secs_f32(),
                bytes_high / 1024,
                bytes_low / 1024
            );
        }
        std::thread::sleep(Duration::from_millis(66).saturating_sub(at.elapsed()));
    }
    pictures.close_camera();
    pictures.reset_decoder();
    PICTURE.lock().unwrap_or_else(|p| p.into_inner()).take();
    RUNNING.store(false, Ordering::Relaxed);
    log::info!("tinyclient: camera loopback ended: {sent} frames encoded, {decoded} decoded");
}
