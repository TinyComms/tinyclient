//! A self-test of the call platform on a real phone or the simulator, run when the app is
//! launched with `-tinySelfTest` (debug builds only). It goes through the same `Platform` mediad
//! uses and logs each step as `selftest:` lines:
//!
//! 1. the microphone permission (the system prompt on first run);
//! 2. the audio session: start, the routes on offer, speaker on and off, stop;
//! 3. the camera (the simulator has none: the words the call screen would show);
//! 4. H.264 through VideoToolbox: synthetic NV12 frames encoded, the Annex B checked for SPS,
//!    PPS and an IDR, then decoded back and the pictures' size checked;
//! 5. ReplayKit's in-app screen capture for three seconds (the system asks first).

use mediad::platform::{CameraRequest, EncoderConfig, Facing, FrameSink, Permission, PixelFormat, Platform, VideoFrame};
use objc2_foundation::NSProcessInfo;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Starts the self-test on a thread of its own when the launch arguments ask for it.
pub(super) fn maybe_start(platform: Arc<dyn Platform>) {
    if !cfg!(debug_assertions) {
        return;
    }
    let arguments: Vec<String> = NSProcessInfo::processInfo().arguments().iter().map(|argument| argument.to_string()).collect();
    if let Some(at) = arguments.iter().position(|argument| argument == "-tinyAuthTest") {
        if let Some(page) = arguments.get(at + 1).cloned() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(4));
                auth(&page);
            });
        }
    }
    if !arguments.iter().any(|argument| argument == "-tinySelfTest") {
        return;
    }
    std::thread::spawn(move || {
        // Let the window come up first, so the system's prompts have something to sit over.
        std::thread::sleep(Duration::from_secs(4));
        run(&*platform);
    });
}

/// The Tiny sign-in's platform half, as chatd drives it: a loopback listener on
/// `127.0.0.1:<port>/callback`, `open_auth` with the provider page `page` (given
/// `redirect_uri` = that callback), then whatever reaches the listener, logged as `selftest:`.
fn auth(page: &str) {
    use std::io::{Read, Write};
    let Some(provider) = protocol::web_sign_in::provider() else {
        return log::info!("selftest: auth FAILED: no web sign-in registered");
    };
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => return log::info!("selftest: auth FAILED: loopback bind: {error}"),
    };
    let port = listener.local_addr().map(|address| address.port()).unwrap_or(0);
    let callback = format!("http://127.0.0.1:{port}/callback");
    let separator = if page.contains('?') { '&' } else { '?' };
    let url = format!("{page}{separator}redirect_uri={callback}&state=selftest-state");
    let request = protocol::web_sign_in::AuthRequest {
        url: url.clone(),
        callback: callback.clone(),
        redirected: Arc::new(|url| log::info!("selftest: auth session handed back {url}")),
    };
    let mut handle = match provider.open_auth(request) {
        Ok(handle) => handle,
        Err(words) => return log::info!("selftest: auth FAILED: {words}"),
    };
    log::info!("selftest: auth session opened at {url}");
    let _ = listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if handle.closed() {
            return log::info!("selftest: auth session closed by the user");
        }
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buffer = [0u8; 2048];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let first = String::from_utf8_lossy(&buffer[..read]).lines().next().unwrap_or("").to_owned();
            let body = "<!doctype html><body style=\"font:20px -apple-system;padding:24px\">Signed in. You can go back to Tiny.</body>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            log::info!("selftest: loopback callback received: {first}");
            std::thread::sleep(Duration::from_secs(3));
            handle.close();
            return log::info!("selftest: auth session closed by the app");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    handle.close();
    log::info!("selftest: auth timed out");
}

fn synthetic(width: u32, height: u32, tick: u32) -> VideoFrame {
    let (w, h) = (width as usize, height as usize);
    let mut data = vec![128u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = ((x + y + tick as usize * 8) % 256) as u8;
        }
    }
    VideoFrame { width, height, format: PixelFormat::Nv12, data, timestamp_us: tick as u64 * 66_000 }
}

fn nal_types(annex_b: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut index = 0;
    while index + 4 < annex_b.len() {
        if annex_b[index..index + 3] == [0, 0, 1] {
            types.push(annex_b[index + 3] & 0x1f);
            index += 3;
        } else {
            index += 1;
        }
    }
    types
}

fn codec(platform: &dyn Platform, width: u32, height: u32, bitrate: u32, screen: bool) {
    let config = EncoderConfig { width, height, bitrate_bps: bitrate, fps: 15, screen };
    let mut encoder = match platform.h264_encoder(config) {
        Ok(encoder) => encoder,
        Err(words) => return log::info!("selftest: h264 {width}x{height} encoder FAILED: {words}"),
    };
    let mut decoder = match platform.h264_decoder() {
        Ok(decoder) => decoder,
        Err(words) => return log::info!("selftest: h264 decoder FAILED: {words}"),
    };
    let (mut bytes, mut decoded, mut idr_with_params, mut sizes_ok) = (0usize, 0usize, 0usize, true);
    for tick in 0..30 {
        let frame = synthetic(width, height, tick);
        let out = match encoder.encode(&frame, tick == 0 || tick == 20) {
            Ok(out) => out,
            Err(words) => return log::info!("selftest: h264 encode FAILED at frame {tick}: {words}"),
        };
        if out.is_empty() {
            continue;
        }
        bytes += out.len();
        let types = nal_types(&out);
        if types.contains(&5) && types.contains(&7) && types.contains(&8) {
            idr_with_params += 1;
        }
        match decoder.decode(&out) {
            Ok(Some(picture)) => {
                decoded += 1;
                sizes_ok &= picture.width == width && picture.height == height && picture.is_whole();
            }
            Ok(None) => {}
            Err(words) => return log::info!("selftest: h264 decode FAILED at frame {tick}: {words}"),
        }
    }
    log::info!(
        "selftest: h264 {width}x{height} {}: 30 frames in, {bytes} bytes Annex B, {idr_with_params} IDR with SPS+PPS, {decoded} decoded, sizes {}",
        if screen { "screen" } else { "camera" },
        if sizes_ok { "match" } else { "MISMATCH" }
    );
}

fn run(platform: &dyn Platform) {
    log::info!("selftest: start");
    match platform.request_permission(Permission::Microphone) {
        Ok(()) => log::info!("selftest: microphone permission granted"),
        Err(words) => log::info!("selftest: microphone permission refused: {words}"),
    }

    match platform.start_audio_session() {
        Ok(()) => {
            log::info!("selftest: audio session active");
            let routes: Vec<&str> = crate::gui::call::phone::hooks::routes().iter().map(|route| route.label()).collect();
            log::info!("selftest: audio routes {routes:?}");
            log::info!("selftest: speaker on: {:?}", platform.set_speaker(true));
            log::info!("selftest: speaker off: {:?}", platform.set_speaker(false));
            platform.stop_audio_session();
            log::info!("selftest: audio session stopped");
        }
        Err(words) => log::info!("selftest: audio session FAILED: {words}"),
    }

    let camera_frames = Arc::new(AtomicUsize::new(0));
    let counter = camera_frames.clone();
    let sink: FrameSink = Arc::new(move |_frame| {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let request = CameraRequest { facing: Facing::Front, width: 640, height: 360, fps: 15 };
    match platform.start_camera(request, sink) {
        Ok(()) => {
            std::thread::sleep(Duration::from_secs(2));
            platform.stop_camera();
            log::info!("selftest: camera ran, {} frames", camera_frames.load(Ordering::Relaxed));
        }
        Err(words) => log::info!("selftest: camera unavailable: {words}"),
    }

    codec(platform, 640, 360, 500_000, false);
    codec(platform, 320, 180, 150_000, false);
    codec(platform, 1280, 720, 1_500_000, true);

    let shared: Arc<Mutex<(usize, Option<(u32, u32, PixelFormat)>)>> = Arc::new(Mutex::new((0, None)));
    let seen = shared.clone();
    let sink: FrameSink = Arc::new(move |frame: VideoFrame| {
        if let Ok(mut seen) = seen.lock() {
            seen.0 += 1;
            seen.1 = Some((frame.width, frame.height, frame.format));
        }
    });
    match platform.start_screen_share(sink) {
        Ok(()) => {
            std::thread::sleep(Duration::from_secs(3));
            platform.stop_screen_share();
            let seen = shared.lock().map(|seen| *seen).unwrap_or((0, None));
            log::info!("selftest: screen share ran, {} frames, last {:?}", seen.0, seen.1);
        }
        Err(words) => log::info!("selftest: screen share FAILED: {words}"),
    }
    log::info!("selftest: done");
}
