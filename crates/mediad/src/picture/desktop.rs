//! The desktop's pictures: OpenH264 (Cisco's binary, fetched at run time), nokhwa's camera on
//! Windows, and the screen through GDI, CoreGraphics or X11. Moved here unchanged from
//! `picture.rs`, which keeps what phones share.

use super::*;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, Level, Profile, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::{OpenH264API, Timestamp};
use openh264_sys2::{SBitrateInfo, ENCODER_OPTION_BITRATE, ENCODER_OPTION_FRAME_RATE, SPATIAL_LAYER_ALL};
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;


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

/// The one decoder a call holds, and the picture it last painted.
#[derive(Default)]
pub struct Incoming {
    decoder: Option<Decoder>,
    canvas: Canvas,
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
        Ok(self.canvas.paint(&colors, width as u32, height as u32))
    }

    /// The file on disk goes.
    pub fn forget(&mut self) {
        self.canvas.forget();
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
