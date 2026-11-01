//! One H.264 picture for a Tiny call: a screen or a camera out, and the one picture in.
//!
//! The SFU takes constrained baseline. A share is one layer, at most 1280×720. A camera is two
//! layers, 640×360 and 320×180, sent with the same RTP time. Both run at 15 frames a second.
//! OpenH264 is Cisco's binary, downloaded from Cisco and loaded when the first picture is
//! encoded, so this crate still builds with no C toolchain. A capture that fails is said in
//! words and the voice call stays. All of it is slow, and none of it runs on the call's tick:
//! `video` holds the threads.

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, Level, Profile, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::{OpenH264API, Timestamp};
use openh264_sys2::{SBitrateInfo, ENCODER_OPTION_BITRATE, ENCODER_OPTION_FRAME_RATE, SPATIAL_LAYER_ALL};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// What an encoder is opened at. `video` halves it when encoding falls behind.
const FULL_FPS: f32 = 15.;

/// A share is fitted inside this. Even, and inside level 3.1.
pub const SHARE_MAX: (u32, u32) = (1280, 720);
/// The camera's high layer.
pub const CAMERA_HIGH: (u32, u32) = (640, 360);
/// The camera's low layer.
pub const CAMERA_LOW: (u32, u32) = (320, 180);

pub const SHARE_BPS: u32 = 1_500_000;
pub const CAMERA_HIGH_BPS: u32 = 500_000;
pub const CAMERA_LOW_BPS: u32 = 150_000;

/// One captured picture, RGBA, top-down, tightly packed.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Windows, macOS and X11 hand back BGRA. A camera hands back RGBA.
    pub bgra: bool,
}

/// I420 planes for one even-sized picture.
pub struct Planes {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl YUVSource for Planes {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// The even size inside `max_w` × `max_h` that keeps the picture's shape.
pub fn fit(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (2, 2);
    }
    let scale = (max_w as f32 / width as f32).min(max_h as f32 / height as f32).min(1.0);
    let even = |value: f32| ((value as u32) & !1).max(2);
    (even(width as f32 * scale), even(height as f32 * scale))
}

/// How far apart the two samples of a scaled pixel are: none at full size, and otherwise enough
/// that a one-pixel line of text falls under one of them.
fn reach(from: usize, to: usize) -> usize {
    if from <= to {
        0
    } else {
        ((from + to) / (2 * to)).max(1)
    }
}

/// RGBA (or BGRA, when `bgra`) scaled to `width` × `height` and stored as I420. Each pixel is the
/// mean of four samples, so a shrunken screen keeps its thin strokes at four times the cost of
/// taking the nearest pixel, whatever the size of the screen.
pub fn i420(src: &[u8], src_w: u32, src_h: u32, width: u32, height: u32, bgra: bool) -> Planes {
    let (width, height) = (width.max(2) as usize, height.max(2) as usize);
    let (src_w, src_h) = (src_w.max(1) as usize, src_h.max(1) as usize);
    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; (width / 2) * (height / 2)];
    let mut v = vec![0u8; (width / 2) * (height / 2)];
    let (across, down) = (reach(src_w, width), reach(src_h, height));
    let sample = |row: usize, col: usize| -> (i32, i32, i32) {
        let at = (row * src_w + col) * 4;
        match src.get(at..at + 3) {
            Some(&[first, g, last]) if bgra => (last as i32, g as i32, first as i32),
            Some(&[first, g, last]) => (first as i32, g as i32, last as i32),
            _ => (0, 0, 0),
        }
    };
    for row in 0..height {
        let top = row * src_h / height;
        let bottom = (top + down).min(src_h - 1);
        for col in 0..width {
            let left = col * src_w / width;
            let right = (left + across).min(src_w - 1);
            let (r, g, b) = if across == 0 && down == 0 {
                sample(top, left)
            } else {
                let (a, b, c, d) = (sample(top, left), sample(top, right), sample(bottom, left), sample(bottom, right));
                ((a.0 + b.0 + c.0 + d.0 + 2) / 4, (a.1 + b.1 + c.1 + d.1 + 2) / 4, (a.2 + b.2 + c.2 + d.2 + 2) / 4)
            };
            y[row * width + col] = clamp(66 * r + 129 * g + 25 * b + 128, 16);
            if row % 2 == 0 && col % 2 == 0 {
                let chroma = (row / 2) * (width / 2) + col / 2;
                u[chroma] = clamp(-38 * r - 74 * g + 112 * b + 128, 128);
                v[chroma] = clamp(112 * r - 94 * g - 18 * b + 128, 128);
            }
        }
    }
    Planes { width, height, y, u, v }
}

fn clamp(sum: i32, bias: i32) -> u8 {
    (sum >> 8).saturating_add(bias).clamp(0, 255) as u8
}

/// One picture on disk for the window, and the file before it gone. Decrypted video: only we may
/// read it, and a name someone else made first is not followed.
fn write_jpeg(rgb: &[u8], width: u32, height: u32, generation: u64) -> io::Result<PathBuf> {
    use std::fs::OpenOptions;
    let path = std::env::temp_dir().join(format!("tinyclient-call-{}-{generation}.jpg", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 70);
    if let Err(err) = encoder.encode(rgb, width, height, image::ExtendedColorType::Rgb8) {
        let _ = std::fs::remove_file(&path);
        return Err(io::Error::other(err));
    }
    Ok(path)
}

/// What one track is asked for, for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ask {
    /// Force a keyframe.
    pub key: bool,
    pub bps: u32,
    /// Since the first capture, on the capture's clock.
    pub at: Duration,
    pub fps: f32,
}

/// One track's frame, Annex B. Empty when the encoder skipped it to keep to its rate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub key: bool,
}

struct Coded {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// What the encoder holds now.
    bps: u32,
    fps: f32,
    /// OpenH264 sets up on the first frame, and takes no options before that.
    begun: bool,
}

impl Coded {
    /// `bps` is also the most this encoder ever spends: [`Coded::tune`] only goes under it.
    fn open(width: u32, height: u32, bps: u32, screen: bool) -> Result<Coded, String> {
        let api = load_api()?;
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps))
            .rate_control_mode(RateControlMode::Bitrate)
            .max_frame_rate(FrameRate::from_hz(FULL_FPS))
            .usage_type(if screen { UsageType::ScreenContentRealTime } else { UsageType::CameraVideoRealTime })
            .profile(Profile::Baseline)
            .level(Level::Level_3_1);
        let encoder = Encoder::with_api_config(api, config).map_err(|err| err.to_string())?;
        Ok(Coded { encoder, width, height, bps, fps: FULL_FPS, begun: false })
    }

    /// The rate and frame rate asked for, if they moved and the encoder can take them.
    fn tune(&mut self, ask: &Ask) {
        if !self.begun {
            return;
        }
        if ask.bps != self.bps && ask.bps > 0 {
            let mut info = SBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: ask.bps as i32 };
            // SAFETY: OpenH264 reads this option as an `SBitrateInfo`, which outlives the call,
            // and copies it. The encoder set up on its first frame.
            let refused = unsafe { self.encoder.raw_api().set_option(ENCODER_OPTION_BITRATE, (&raw mut info).cast()) };
            if refused == 0 {
                self.bps = ask.bps;
            } else {
                tracing::warn!("mediad: the encoder refused a bitrate of {}: {refused}", ask.bps);
            }
        }
        if ask.fps != self.fps && ask.fps > 0. {
            let mut fps = ask.fps;
            // SAFETY: as above, with a float, which is what this option is read as.
            let refused = unsafe { self.encoder.raw_api().set_option(ENCODER_OPTION_FRAME_RATE, (&raw mut fps).cast()) };
            if refused == 0 {
                self.fps = ask.fps;
            }
        }
    }

    fn encode(&mut self, planes: &Planes, ask: &Ask) -> Result<Encoded, String> {
        self.tune(ask);
        if ask.key && self.begun {
            self.encoder.force_intra_frame();
        }
        let stamp = Timestamp::from_millis(ask.at.as_millis() as u64);
        let encoded = self.encoder.encode_at(planes, stamp).map_err(|err| err.to_string())?;
        let key = matches!(encoded.frame_type(), FrameType::IDR | FrameType::I);
        let bytes = encoded.to_vec();
        self.begun = true;
        Ok(Encoded { bytes, key })
    }
}

/// The encoder for one track at this size: the one it has, or a new one, which starts with a
/// keyframe.
fn coded(slot: &mut Option<Coded>, width: u32, height: u32, nominal: u32, screen: bool) -> Result<&mut Coded, String> {
    if !slot.as_ref().is_some_and(|coded| coded.width == width && coded.height == height) {
        *slot = Some(Coded::open(width, height, nominal, screen)?);
    }
    slot.as_mut().ok_or_else(|| "No encoder.".to_owned())
}

/// The encoders and the capture of what a call sends. Built on the first picture, not at join.
#[derive(Default)]
pub struct Outgoing {
    share: Option<Coded>,
    high: Option<Coded>,
    low: Option<Coded>,
    camera: Camera,
}

impl Outgoing {
    /// One share frame, or why the screen could not be read.
    pub fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
        // A capture is wasted until the codec loads.
        if self.share.is_none() {
            load_api()?;
        }
        let grabbed = capture_screen()?;
        let (width, height) = fit(grabbed.width, grabbed.height, SHARE_MAX.0, SHARE_MAX.1);
        let planes = i420(&grabbed.pixels, grabbed.width, grabbed.height, width, height, grabbed.bgra);
        coded(&mut self.share, width, height, SHARE_BPS, true)?.encode(&planes, &ask)
    }

    /// The camera's two layers, high then low, or why the camera could not be read. A layer with
    /// no `Ask` is stopped: nothing is spent on it and it comes back empty.
    pub fn camera(&mut self, high: Option<Ask>, low: Option<Ask>) -> Result<(Encoded, Encoded), String> {
        if self.high.is_none() || self.low.is_none() {
            load_api()?;
        }
        let grabbed = self.camera.frame()?;
        let layer = |slot: &mut Option<Coded>, ask: Option<Ask>, most: (u32, u32), nominal: u32| match ask {
            Some(ask) => {
                let (width, height) = fit(grabbed.width, grabbed.height, most.0, most.1);
                let planes = i420(&grabbed.pixels, grabbed.width, grabbed.height, width, height, grabbed.bgra);
                coded(slot, width, height, nominal, false)?.encode(&planes, &ask)
            }
            None => Ok(Encoded::default()),
        };
        let high = layer(&mut self.high, high, CAMERA_HIGH, CAMERA_HIGH_BPS)?;
        let low = layer(&mut self.low, low, CAMERA_LOW, CAMERA_LOW_BPS)?;
        Ok((high, low))
    }

    pub fn close_camera(&mut self) {
        self.camera.close();
    }
}

/// One picture on disk for the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
}

/// The one decoder a call holds, and the picture it last painted.
#[derive(Default)]
pub struct Incoming {
    decoder: Option<Decoder>,
    generation: u64,
    /// The file on disk now, removed when the next is written or the picture stops.
    last: Option<PathBuf>,
}

impl Incoming {
    /// Drops a decoder whose sender changed, so the next frame starts from a keyframe.
    pub fn reset(&mut self) {
        self.decoder = None;
    }

    /// Decodes one received Annex B frame, and paints it if `paint`. Every frame goes through the
    /// decoder, painted or not: the next one is built on it. `Err` means it did not decode;
    /// `Ok(None)` that there is nothing to paint.
    #[allow(clippy::result_unit_err)]
    pub fn decode(&mut self, annex_b: &[u8], paint: bool) -> Result<Option<Painted>, ()> {
        if self.decoder.is_none() {
            let api = load_api().map_err(|_| ())?;
            self.decoder = Decoder::with_api_config(api, DecoderConfig::new()).ok();
        }
        let decoder = self.decoder.as_mut().ok_or(())?;
        let Some(decoded) = decoder.decode(annex_b).map_err(|_| ())? else {
            return Ok(None);
        };
        if !paint {
            return Ok(None);
        }
        let (width, height) = decoded.dimensions();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let mut colors = vec![0u8; width * height * 3];
        // This honours the decoder's strides, which are wider than the picture.
        decoded.write_rgb8(&mut colors);
        Ok(self.paint(&colors, width as u32, height as u32))
    }

    fn paint(&mut self, colors: &[u8], width: u32, height: u32) -> Option<Painted> {
        let generation = self.generation.wrapping_add(1);
        let path = write_jpeg(colors, width, height, generation).ok()?;
        self.generation = generation;
        self.forget();
        self.last = Some(path.clone());
        Some(Painted { path, width, height, generation })
    }

    /// The file on disk goes.
    pub fn forget(&mut self) {
        if let Some(last) = self.last.take() {
            let _ = std::fs::remove_file(last);
        }
    }
}

/// One try of the screen, so a button can say no before the call claims to be sharing.
pub fn probe_screen() -> Result<(), String> {
    capture_screen().map(|_| ())
}

#[derive(Default)]
struct Camera {
    #[cfg(windows)]
    inner: Option<nokhwa::Camera>,
}

impl Camera {
    fn frame(&mut self) -> Result<Rgba, String> {
        #[cfg(windows)]
        {
            self.open()?;
            let camera = self.inner.as_mut().ok_or_else(|| "No camera was opened.".to_owned())?;
            let frame = camera.frame().map_err(|err| err.to_string())?;
            let image = frame.decode_image::<nokhwa::pixel_format::RgbFormat>().map_err(|err| err.to_string())?;
            let width = image.width();
            let height = image.height();
            let mut pixels = Vec::with_capacity((width * height * 4) as usize);
            for pixel in image.pixels() {
                pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
            return Ok(Rgba { width, height, pixels, bgra: false });
        }
        #[cfg(not(windows))]
        {
            Err("A camera isn't available on this system yet. You can still share your screen.".to_owned())
        }
    }

    #[cfg(windows)]
    fn open(&mut self) -> Result<(), String> {
        if self.inner.is_some() {
            return Ok(());
        }
        use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
        use nokhwa::pixel_format::RgbFormat;
        let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let mut camera = nokhwa::Camera::new(CameraIndex::Index(0), format).map_err(|err| err.to_string())?;
        camera.open_stream().map_err(|err| err.to_string())?;
        self.inner = Some(camera);
        Ok(())
    }

    fn close(&mut self) {
        #[cfg(windows)]
        {
            if let Some(camera) = self.inner.as_mut() {
                let _ = camera.stop_stream();
            }
            self.inner = None;
        }
    }
}

/// The primary screen as RGBA or BGRA. The whole screen, which the share then fits.
pub fn capture_screen() -> Result<Rgba, String> {
    #[cfg(windows)]
    {
        return capture_screen_windows();
    }
    #[cfg(target_os = "macos")]
    {
        return capture_screen_macos();
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return capture_screen_linux();
    }
    #[cfg(not(any(windows, target_os = "macos", all(target_os = "linux", not(target_env = "ohos")))))]
    {
        Err("Screen sharing isn't available on this system.".to_owned())
    }
}

#[cfg(windows)]
fn capture_screen_windows() -> Result<Rgba, String> {
    use std::mem::size_of;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            return Err("The screen has no size.".to_owned());
        }
        let desktop = GetDesktopWindow();
        let dc = GetDC(desktop);
        if dc.is_null() {
            return Err("tinyclient can't record the screen.".to_owned());
        }
        let memory = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, width, height);
        let previous = SelectObject(memory, bitmap as _);
        let copied = BitBlt(memory, 0, 0, width, height, dc, 0, 0, SRCCOPY);
        let mut header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let rows = GetDIBits(memory, bitmap, 0, height as u32, pixels.as_mut_ptr().cast(), (&mut header as *mut BITMAPINFOHEADER).cast(), DIB_RGB_COLORS);
        SelectObject(memory, previous);
        DeleteObject(bitmap as _);
        DeleteDC(memory);
        ReleaseDC(desktop, dc);
        if copied == 0 || rows == 0 {
            return Err("tinyclient can't record the screen.".to_owned());
        }
        Ok(Rgba { width: width as u32, height: height as u32, pixels, bgra: true })
    }
}

#[cfg(target_os = "macos")]
fn capture_screen_macos() -> Result<Rgba, String> {
    use core_graphics::display::CGDisplay;
    let image = CGDisplay::main().image().ok_or_else(|| {
        "tinyclient can't record the screen. Allow it in System Settings › Privacy & Security › Screen & System Audio Recording.".to_owned()
    })?;
    let width = image.width() as u32;
    let height = image.height() as u32;
    let stride = image.bytes_per_row();
    let data = image.data();
    let bytes = data.bytes();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * stride;
        let row_bytes = bytes.get(start..start + width as usize * 4).ok_or_else(|| "The screen image was short.".to_owned())?;
        pixels.extend_from_slice(row_bytes);
    }
    Ok(Rgba { width, height, pixels, bgra: true })
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn capture_screen_linux() -> Result<Rgba, String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    let (conn, screen_num) = x11rb::connect(None).map_err(|_| "This session has no X11 display, so the screen can't be shared.".to_owned())?;
    let screen = &conn.setup().roots[screen_num];
    let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, screen.root, 0, 0, width, height, !0)
        .map_err(|err| err.to_string())?
        .reply()
        .map_err(|_| "The screen could not be read.".to_owned())?;
    if reply.depth < 24 {
        return Err("This screen's picture is in a form tinyclient can't share.".to_owned());
    }
    Ok(Rgba { width: width as u32, height: height as u32, pixels: reply.data, bgra: true })
}

/// Said while the binary is on its way, so a call does not treat the wait as a failure.
pub const CODEC_LATER: &str = "OpenH264 is still downloading.";
const DOWNLOAD_FAILED: &str = "OpenH264 could not be downloaded. The call stays a voice call.";

enum Fetch {
    Idle,
    Busy,
    Failed(String),
}

fn fetch() -> &'static Mutex<Fetch> {
    static FETCH: OnceLock<Mutex<Fetch>> = OnceLock::new();
    FETCH.get_or_init(|| Mutex::new(Fetch::Idle))
}

/// Starts the download when the binary is not here yet, and returns at once.
pub fn warm() {
    let Ok(path) = library_path() else {
        return;
    };
    if path.exists() {
        return;
    }
    start_download(path);
}

/// A failed download may be tried again, the next time share or camera is turned on.
pub fn retry_codec() {
    let Ok(mut state) = fetch().lock() else {
        return;
    };
    if matches!(*state, Fetch::Failed(_)) {
        *state = Fetch::Idle;
    }
}

fn start_download(path: PathBuf) {
    let Ok(mut state) = fetch().lock() else {
        return;
    };
    if !matches!(*state, Fetch::Idle) {
        return;
    }
    *state = Fetch::Busy;
    drop(state);
    let started = std::thread::Builder::new().name("openh264".into()).spawn(move || {
        let result = download(&path);
        if let Ok(mut state) = fetch().lock() {
            *state = match result {
                Ok(()) => Fetch::Idle,
                Err(err) => Fetch::Failed(err),
            };
        }
    });
    if started.is_err() {
        if let Ok(mut state) = fetch().lock() {
            *state = Fetch::Failed(DOWNLOAD_FAILED.to_owned());
        }
    }
}

/// Cisco's OpenH264 2.6.0 for this machine. A download in progress is [`CODEC_LATER`], not a
/// failure, and this never blocks on the network.
fn load_api() -> Result<OpenH264API, String> {
    let path = library_path()?;
    if let Some(api) = open_cached(&path) {
        return Ok(api);
    }
    match fetch().lock() {
        Ok(state) => match &*state {
            Fetch::Busy => return Err(CODEC_LATER.to_owned()),
            Fetch::Failed(err) => return Err(err.clone()),
            Fetch::Idle => {}
        },
        Err(_) => return Err(DOWNLOAD_FAILED.to_owned()),
    }
    start_download(path);
    Err(CODEC_LATER.to_owned())
}

fn open_cached(path: &Path) -> Option<OpenH264API> {
    if !path.exists() {
        return None;
    }
    match OpenH264API::from_blob_path(path) {
        Ok(api) => Some(api),
        Err(_) => {
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn library_path() -> Result<PathBuf, String> {
    let name = library_name().ok_or_else(|| "OpenH264 has no build for this system.".to_owned())?;
    let dir = if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("tinyclient")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("tinyclient")
    } else {
        std::env::temp_dir().join("tinyclient")
    };
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join(name))
}

fn library_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("openh264-2.6.0-win64.dll")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("openh264-2.6.0-win-arm64.dll")
    } else if cfg!(all(windows, target_arch = "x86")) {
        Some("openh264-2.6.0-win32.dll")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("libopenh264-2.6.0-linux64.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("libopenh264-2.6.0-linux-arm64.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        Some("libopenh264-2.6.0-linux-arm.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "x86")) {
        Some("libopenh264-2.6.0-linux32.8.so")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("libopenh264-2.6.0-mac-arm64.dylib")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("libopenh264-2.6.0-mac-x64.dylib")
    } else {
        None
    }
}

fn library_url(name: &str) -> String {
    format!("https://ciscobinary.openh264.org/{name}.bz2")
}

fn download(path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| "OpenH264 has no name.".to_owned())?;
    let url = library_url(name);
    let response = ureq::get(&url).timeout(Duration::from_secs(20)).call().map_err(|err| {
        tracing::warn!("mediad: OpenH264 download failed: {err}");
        DOWNLOAD_FAILED.to_owned()
    })?;
    let mut packed = Vec::new();
    response.into_reader().read_to_end(&mut packed).map_err(|err| err.to_string())?;
    let mut plain = Vec::new();
    bzip2_rs::DecoderReader::new(&packed[..]).read_to_end(&mut plain).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &plain).map_err(|err| err.to_string())?;
    std::fs::rename(&tmp, path).map_err(|err| err.to_string())?;
    Ok(())
}

/// Whether a camera can be opened, said before the button claims it is on.
pub fn probe_camera() -> Result<(), String> {
    let mut camera = Camera::default();
    camera.frame().map(|_| ())?;
    camera.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openh264_is_fetched_from_cisco() {
        let name = library_name().unwrap_or("openh264-2.6.0-win64.dll");
        let url = library_url(name);
        assert!(url.starts_with("https://ciscobinary.openh264.org/"), "{url}");
        assert!(url.ends_with(".bz2"), "{url}");
        assert!(url.contains(name), "{url}");
        assert!(!url.contains("github.com"), "{url}");
    }

    #[test]
    fn a_picture_fits_inside_the_cap_and_stays_even() {
        assert_eq!(fit(1920, 1080, 1280, 720), (1280, 720));
        assert_eq!(fit(800, 800, 1280, 720), (720, 720));
        assert_eq!(fit(100, 50, 1280, 720), (100, 50));
        let (width, height) = fit(1919, 1079, 1280, 720);
        assert_eq!(width % 2, 0);
        assert_eq!(height % 2, 0);
        assert!(width <= 1280 && height <= 720);
    }

    #[test]
    fn a_red_pixel_is_red_in_i420() {
        let rgba = [255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let planes = i420(&rgba, 2, 2, 2, 2, false);
        assert!((planes.y[0] as i16 - 81).abs() < 8, "y {}", planes.y[0]);
        assert!(planes.v[0] > 200, "v {}", planes.v[0]);
        assert!(planes.u[0] < 128, "u {}", planes.u[0]);
    }

    /// Text shrunk from 1080p to 720p: nearest-neighbour takes two rows in three, so it loses
    /// any one-pixel stroke that falls on the third.
    #[test]
    fn a_shrunken_screen_keeps_a_one_pixel_line() {
        for line in 0..6 {
            let (width, height) = (6usize, 6usize);
            let mut rgba = vec![255u8; width * height * 4];
            rgba[line * width * 4..(line + 1) * width * 4].fill(0);
            let planes = i420(&rgba, 6, 6, 4, 4, false);
            let darkest = planes.y.iter().copied().min().unwrap();
            assert!(darkest < 150, "line {line} vanished: darkest {darkest}");
        }
    }

    #[test]
    fn a_picture_at_its_own_size_is_left_exactly_as_it_is() {
        let rgba: Vec<u8> = [[0u8, 0, 0, 255], [255, 255, 255, 255]].repeat(8).concat();
        let planes = i420(&rgba, 4, 4, 4, 4, false);
        assert_eq!(planes.y[0], 16);
        assert_eq!(planes.y[1], 235);
    }

    #[test]
    fn a_short_grab_scales_as_black_and_does_not_panic() {
        let planes = i420(&[255; 8], 4, 4, 2, 2, false);
        assert_eq!(planes.y[2], 16);
    }

    #[test]
    fn a_failed_decode_leaves_no_file_behind_and_stopping_takes_the_last_one() {
        let mut incoming = Incoming { generation: u64::from(std::process::id()) * 1000, ..Incoming::default() };
        let colors = [128u8; 2 * 2 * 3];
        let first = incoming.paint(&colors, 2, 2).unwrap();
        assert!(first.path.exists());
        // A frame in between decoded to nothing, so its generation never reached the disk.
        incoming.generation += 1;
        let second = incoming.paint(&colors, 2, 2).unwrap();
        assert!(!first.path.exists(), "the file before the gap went too");
        assert!(second.path.exists());
        incoming.forget();
        assert!(!second.path.exists());
    }

    /// A camera, or a screen, through real OpenH264: `cargo test -p mediad --lib -- --ignored`
    /// on a machine that has the binary.
    #[test]
    #[ignore = "needs the OpenH264 binary, which is not in the tree"]
    fn frames_survive_the_real_codec_in_bitrate_mode_and_the_rate_moves() {
        let (width, height) = (640usize, 360usize);
        let mut outgoing = Coded::open(width as u32, height as u32, CAMERA_HIGH_BPS, false).unwrap();
        let mut incoming = Incoming::default();
        let (mut key, mut bytes_high, mut bytes_low) = (false, 0usize, 0usize);
        for n in 0..90usize {
            // Noise that moves, so the encoder has to work for its bits.
            let mut rgba = vec![0u8; width * height * 4];
            for (at, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = ((at * 2654435761 + n * 40503) >> 7) as u8;
                pixel.copy_from_slice(&[value, value / 2, 255 - value, 255]);
            }
            let planes = i420(&rgba, width as u32, height as u32, width as u32, height as u32, false);
            let bps = if n < 45 { CAMERA_HIGH_BPS } else { CAMERA_LOW_BPS };
            let ask = Ask { key: n == 30, bps, at: Duration::from_millis(n as u64 * 66), fps: FULL_FPS };
            let encoded = outgoing.encode(&planes, &ask).unwrap();
            if n == 0 || n == 30 {
                assert!(encoded.key, "frame {n} was asked to be a keyframe");
                key = true;
            }
            if n > 0 && n != 30 {
                assert!(!encoded.key, "uiIntraPeriod 0 makes no keyframe of its own at {n}");
            }
            if (5..45).contains(&n) && n != 30 {
                bytes_high += encoded.bytes.len();
            }
            if n >= 55 {
                bytes_low += encoded.bytes.len();
            }
            if !encoded.bytes.is_empty() {
                let painted = incoming.decode(&encoded.bytes, n == 0).expect("every frame decodes");
                assert_eq!(painted.is_some(), n == 0, "only what was asked is painted, at frame {n}");
            }
        }
        incoming.forget();
        assert!(key);
        assert!(bytes_low * 45 / 35 < bytes_high, "the rate moved: {bytes_high} then {bytes_low}");
    }
}
