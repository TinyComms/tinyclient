//! One H.264 picture for a Tiny call: a screen or a camera out, and the one picture in.
//!
//! The SFU takes constrained baseline. A share is one layer, at most 1280×720. A camera is two
//! layers, 640×360 and 320×180, sent with the same RTP time. Both run at 15 frames a second.
//! OpenH264 is Cisco's binary, downloaded from Cisco and loaded when the first picture is
//! encoded, so this crate still builds with no C toolchain. A capture that fails is said in
//! words and the voice call stays.

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, Level, Profile, UsageType};
use openh264::formats::YUVSource;
use openh264::OpenH264API;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// A share is fitted inside this. Even, and inside level 3.1.
pub const SHARE_MAX: (u32, u32) = (1280, 720);
/// The camera's high layer.
pub const CAMERA_HIGH: (u32, u32) = (640, 360);
/// The camera's low layer.
pub const CAMERA_LOW: (u32, u32) = (320, 180);

const SHARE_BPS: u32 = 1_500_000;
const CAMERA_HIGH_BPS: u32 = 500_000;
const CAMERA_LOW_BPS: u32 = 150_000;

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

/// RGBA (or BGRA, when `bgra`) scaled to `width` × `height` and stored as I420.
pub fn i420(src: &[u8], src_w: u32, src_h: u32, width: u32, height: u32, bgra: bool) -> Planes {
    let (width, height) = (width.max(2) as usize, height.max(2) as usize);
    let (src_w, src_h) = (src_w.max(1) as usize, src_h.max(1) as usize);
    let mut y = vec![0u8; width * height];
    let mut u = vec![0u8; (width / 2) * (height / 2)];
    let mut v = vec![0u8; (width / 2) * (height / 2)];
    for row in 0..height {
        let sy = row * src_h / height;
        for col in 0..width {
            let sx = col * src_w / width;
            let at = (sy * src_w + sx) * 4;
            let (r, g, b) = if at + 2 >= src.len() {
                (0, 0, 0)
            } else if bgra {
                (src[at + 2], src[at + 1], src[at])
            } else {
                (src[at], src[at + 1], src[at + 2])
            };
            y[row * width + col] = clamp(66 * r as i32 + 129 * g as i32 + 25 * b as i32 + 128, 16);
            if row % 2 == 0 && col % 2 == 0 {
                let chroma = (row / 2) * (width / 2) + col / 2;
                u[chroma] = clamp(-38 * r as i32 - 74 * g as i32 + 112 * b as i32 + 128, 128);
                v[chroma] = clamp(112 * r as i32 - 94 * g as i32 - 18 * b as i32 + 128, 128);
            }
        }
    }
    Planes { width, height, y, u, v }
}

fn clamp(sum: i32, bias: i32) -> u8 {
    (sum >> 8).saturating_add(bias).clamp(0, 255) as u8
}

/// RGB from I420, tightly packed.
pub fn rgb(planes: &Planes) -> Vec<u8> {
    let (w, h) = (planes.width, planes.height);
    let mut out = vec![0u8; w * h * 3];
    for row in 0..h {
        for col in 0..w {
            let y = planes.y[row * w + col] as i32;
            let chroma = (row / 2) * (w / 2) + col / 2;
            let u = planes.u[chroma] as i32 - 128;
            let v = planes.v[chroma] as i32 - 128;
            let c = y - 16;
            let r = (298 * c + 409 * v + 128) >> 8;
            let g = (298 * c - 100 * u - 208 * v + 128) >> 8;
            let b = (298 * c + 516 * u + 128) >> 8;
            let at = (row * w + col) * 3;
            out[at] = r.clamp(0, 255) as u8;
            out[at + 1] = g.clamp(0, 255) as u8;
            out[at + 2] = b.clamp(0, 255) as u8;
        }
    }
    out
}

/// Writes one JPEG and removes the previous generation, so the window does not keep a stale file.
pub fn write_jpeg(rgb: &[u8], width: u32, height: u32, generation: u64) -> io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("tinyclient-call-{generation}.jpg"));
    let file = std::fs::File::create(&path)?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 70);
    encoder
        .encode(rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|err| io::Error::other(err))?;
    if generation > 0 {
        let previous = std::env::temp_dir().join(format!("tinyclient-call-{}.jpg", generation - 1));
        let _ = std::fs::remove_file(previous);
    }
    Ok(path)
}

struct Coded {
    encoder: Encoder,
    width: u32,
    height: u32,
}

impl Coded {
    fn open(width: u32, height: u32, bps: u32, screen: bool) -> Result<Coded, String> {
        let api = load_api()?;
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(15.0))
            .usage_type(if screen { UsageType::ScreenContentRealTime } else { UsageType::CameraVideoRealTime })
            .profile(Profile::Baseline)
            .level(Level::Level_3_1);
        let encoder = Encoder::with_api_config(api, config).map_err(|err| err.to_string())?;
        Ok(Coded { encoder, width, height })
    }

    fn encode(&mut self, planes: &Planes, key: bool) -> Result<Vec<u8>, String> {
        if key {
            self.encoder.force_intra_frame();
        }
        let encoded = self.encoder.encode(planes).map_err(|err| err.to_string())?;
        Ok(encoded.to_vec())
    }
}

/// The encoders and the one decoder a call holds. Built on the first picture, not at join.
pub struct Pictures {
    share: Option<Coded>,
    high: Option<Coded>,
    low: Option<Coded>,
    decoder: Option<Decoder>,
    camera: Camera,
}

impl Default for Pictures {
    fn default() -> Self {
        Self { share: None, high: None, low: None, decoder: None, camera: Camera::default() }
    }
}

impl Pictures {
    /// One share frame, Annex B, or why the screen could not be read.
    pub fn share(&mut self, key: bool) -> Result<Vec<u8>, String> {
        // The download must not sit on the call thread, and a capture is wasted until it loads.
        if self.share.is_none() {
            load_api()?;
        }
        let grabbed = capture_screen()?;
        let (width, height) = fit(grabbed.width, grabbed.height, SHARE_MAX.0, SHARE_MAX.1);
        let planes = i420(&grabbed.pixels, grabbed.width, grabbed.height, width, height, grabbed.bgra);
        let coded = match &mut self.share {
            Some(coded) if coded.width == width && coded.height == height => coded,
            slot => {
                *slot = Some(Coded::open(width, height, SHARE_BPS, true)?);
                slot.as_mut().unwrap()
            }
        };
        coded.encode(&planes, key)
    }

    /// The camera's two layers, high then low, or why the camera could not be read.
    pub fn camera(&mut self, key: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
        if self.high.is_none() || self.low.is_none() {
            load_api()?;
        }
        let grabbed = self.camera.frame()?;
        let (hw, hh) = fit(grabbed.width, grabbed.height, CAMERA_HIGH.0, CAMERA_HIGH.1);
        let (lw, lh) = fit(grabbed.width, grabbed.height, CAMERA_LOW.0, CAMERA_LOW.1);
        let high = i420(&grabbed.pixels, grabbed.width, grabbed.height, hw, hh, grabbed.bgra);
        let low = i420(&grabbed.pixels, grabbed.width, grabbed.height, lw, lh, grabbed.bgra);
        let high_coded = match &mut self.high {
            Some(coded) if coded.width == hw && coded.height == hh => coded,
            slot => {
                *slot = Some(Coded::open(hw, hh, CAMERA_HIGH_BPS, false)?);
                slot.as_mut().unwrap()
            }
        };
        let high_bytes = high_coded.encode(&high, key)?;
        let low_coded = match &mut self.low {
            Some(coded) if coded.width == lw && coded.height == lh => coded,
            slot => {
                *slot = Some(Coded::open(lw, lh, CAMERA_LOW_BPS, false)?);
                slot.as_mut().unwrap()
            }
        };
        Ok((high_bytes, low_coded.encode(&low, key)?))
    }

    /// Drops a decoder whose sender changed, so the next frame starts from a keyframe.
    pub fn reset_decoder(&mut self) {
        self.decoder = None;
    }

    /// One received Annex B frame as a JPEG path, once it decodes.
    pub fn decode(&mut self, annex_b: &[u8], generation: u64) -> Option<(PathBuf, u32, u32)> {
        if self.decoder.is_none() {
            let api = load_api().ok()?;
            self.decoder = Decoder::with_api_config(api, DecoderConfig::new()).ok();
        }
        let decoder = self.decoder.as_mut()?;
        let decoded = decoder.decode(annex_b).ok().flatten()?;
        let (width, height) = decoded.dimensions();
        if width == 0 || height == 0 {
            return None;
        }
        let planes = Planes {
            width,
            height,
            y: decoded.y().to_vec(),
            u: decoded.u().to_vec(),
            v: decoded.v().to_vec(),
        };
        let colors = rgb(&planes);
        let path = write_jpeg(&colors, width as u32, height as u32, generation).ok()?;
        Some((path, width as u32, height as u32))
    }

    pub fn close_camera(&mut self) {
        self.camera.close();
    }
}

/// One try of the screen, so a button can say no before the call claims to be sharing.
pub fn probe_screen() -> Result<(), String> {
    capture_screen().map(|_| ())
}

struct Camera {
    #[cfg(windows)]
    inner: Option<nokhwa::Camera>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            inner: None,
        }
    }
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
    fn a_red_pixel_is_red_in_i420_and_back_in_rgb() {
        let rgba = [255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let planes = i420(&rgba, 2, 2, 2, 2, false);
        assert!((planes.y[0] as i16 - 81).abs() < 8, "y {}", planes.y[0]);
        assert!(planes.v[0] > 200, "v {}", planes.v[0]);
        let colors = rgb(&planes);
        assert!(colors[0] > 200 && colors[1] < 40 && colors[2] < 40, "{colors:?}");
    }
}
