//! One H.264 picture for a Tiny call: a screen or a camera out, and the one picture in.
//!
//! The SFU takes constrained baseline. A share is one layer, at the size, rate and bitrate the
//! sharer chose ([`Quality`]): the screen as it is up to 2560×1440 at up to 60 frames a second,
//! or less. A camera is two layers, 640×360 and 320×180, sent with the same RTP time, at 15
//! frames a second.
//! On a desktop (`desktop`) OpenH264 is Cisco's binary, downloaded from Cisco and loaded when the first picture is
//! encoded, so this crate still builds with no C toolchain. The camera is the system's own
//! (Media Foundation, AVFoundation, V4L2), and on Linux a camera V4L2 cannot open is asked for
//! through the desktop portal. The screen is GDI or Desktop Duplication on Windows, Core Graphics
//! on macOS, the X11 root on an X11 session, and that same portal on Wayland. A capture that fails
//! is said in words and the voice call stays. A phone captures and codes through its `platform`
//! (`phone`). All of it is slow, and none of it runs on the call's tick: `video` holds the threads.

use protocol::{PictureKind, ShareQuality};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Said while the binary is on its way, so a call does not treat the wait as a failure.
pub const CODEC_LATER: &str = "OpenH264 is still downloading.";

// Desktop: OpenH264 and the desktop's own camera and screen. Phones: `platform`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use desktop::*;
/// What a share sends: the picker's list and the one chosen.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod screens;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use screens::{choose_screen, screens};
/// A phone offers Settings no camera list: its own picker chooses, and `--cameras` prints none.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn cameras(_folder: Option<&std::path::Path>) -> Vec<protocol::CameraSource> {
    Vec::new()
}
/// A screen through Desktop Duplication, which is what a share at 60 frames a second needs.
#[cfg(windows)]
mod duplicate;
/// PipeWire's SPA pods and the struct layout a `dlopen` has to match, plus which Linux session
/// this is. Compiled into the tests on every desktop; only a Linux build loads PipeWire.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
mod spa_pod;
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
mod pipewire_abi;
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
mod session;
/// xdg-desktop-portal ScreenCast and Camera. One path for every desktop that ships a portal.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod portal;

/// The camera the next camera frame comes from, as `settings::CAMERA` stores it (a name, then a
/// tab and the system's id); `None` is the first one listed. One choice per process.
static CAMERA: Mutex<Option<String>> = Mutex::new(None);

/// What the camera opens from its next frame on. One already open is closed and opened again.
pub fn choose_camera(choice: Option<String>) {
    *CAMERA.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = choice;
}

pub(crate) fn camera_chosen() -> Option<String> {
    CAMERA.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// The size my own camera is shown to me at. Smaller than what is sent: it is a corner.
pub const SELF_VIEW: (u32, u32) = (640, 360);
/// How often my own camera is painted for me.
pub const SELF_VIEW_EVERY: Duration = Duration::from_millis(100);
pub mod convert;
/// Built everywhere, so host tests drive it with `platform::fake`.
pub mod phone;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub use phone::*;

/// A phone's share is fitted inside this, and so is a desktop's at `Low`. Even, and inside
/// level 3.1, which is what a phone's hardware codec is sure to take.
pub const SHARE_MAX: (u32, u32) = (1280, 720);
/// The camera's high layer.
pub const CAMERA_HIGH: (u32, u32) = (640, 360);
/// The camera's low layer.
pub const CAMERA_LOW: (u32, u32) = (320, 180);

/// A phone's share, and a desktop's at `Low`.
pub const SHARE_BPS: u32 = 1_500_000;
pub const CAMERA_HIGH_BPS: u32 = 500_000;
pub const CAMERA_LOW_BPS: u32 = 150_000;

/// What a camera's encoder is opened at. `video` halves it when encoding falls behind.
pub(crate) const FULL_FPS: f32 = 15.;
/// The most a share is captured at, whatever the screen refreshes at. The capture, the
/// conversion and the encoder are all on the CPU; above this they could not keep up anyway.
pub const SHARE_FPS_MOST: u32 = 60;

/// What a share spends, by the sharer's choice ([`protocol::ShareQuality`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    /// The share is fitted inside this.
    pub max: (u32, u32),
    /// Frames a second, at most: the screen's own rate caps it too.
    pub fps: u32,
    /// Bits a second, at most.
    pub bps: u32,
}

impl Quality {
    pub const fn of(quality: ShareQuality) -> Quality {
        match quality {
            ShareQuality::Best => Quality { max: (2560, 1440), fps: SHARE_FPS_MOST, bps: 8_000_000 },
            ShareQuality::Balanced => Quality { max: (1920, 1080), fps: 30, bps: 4_000_000 },
            ShareQuality::Low => Quality { max: SHARE_MAX, fps: 15, bps: SHARE_BPS },
        }
    }
}

/// The quality the next share goes at. One call per process, so one choice per process. A
/// phone has no choice: its hardware codec gets `Low`.
static QUALITY: AtomicU8 = AtomicU8::new(ShareQuality::Best as u8);

/// What a share spends from its next frame on. On a desktop only: see [`share_quality`].
pub fn choose_quality(quality: Option<ShareQuality>) {
    QUALITY.store(quality.unwrap_or_default() as u8, Ordering::Relaxed);
}

/// Frames a second for the share now: what was chosen, held to the shared screen's own refresh
/// rate where that is known. Nothing new is on a screen between its refreshes.
pub fn share_fps() -> u32 {
    let chosen = share_quality().fps;
    #[cfg(windows)]
    {
        screens::refresh_rate(screens::chosen().as_deref()).map_or(chosen, |hz| chosen.min(hz)).max(1)
    }
    #[cfg(not(windows))]
    {
        chosen.max(1)
    }
}

/// What the share spends now.
pub fn share_quality() -> Quality {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Quality::of(ShareQuality::Low)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        Quality::of(match QUALITY.load(Ordering::Relaxed) {
            1 => ShareQuality::Balanced,
            2 => ShareQuality::Low,
            _ => ShareQuality::Best,
        })
    }
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

/// One captured picture, RGBA, top-down, tightly packed.
#[derive(Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Windows, macOS, X11 and a portal screen hand back BGRA. A camera hands back RGBA, unless the portal's camera negotiated BGRA.
    pub bgra: bool,
}

/// I420 planes for one even-sized picture.
pub struct Planes {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) y: Vec<u8>,
    pub(crate) u: Vec<u8>,
    pub(crate) v: Vec<u8>,
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
    let scale = Scale { src, src_w, src_h, width, height, bgra, across, down };
    // Bands of rows, an even number each so a band owns whole chroma rows, each converted on a
    // thread of its own: one thread took longer than a frame at 60 on a 1440p screen.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let rows_per = (height.div_ceil(threads).max(2) + 1) & !1;
    let mut bands = Vec::with_capacity(threads);
    let (mut y_rest, mut u_rest, mut v_rest) = (&mut y[..], &mut u[..], &mut v[..]);
    let mut first = 0;
    while first < height {
        let rows = rows_per.min(height - first);
        let chroma = (rows.div_ceil(2) * (width / 2)).min(u_rest.len());
        let (y_band, y_next) = y_rest.split_at_mut(rows * width);
        let (u_band, u_next) = u_rest.split_at_mut(chroma);
        let (v_band, v_next) = v_rest.split_at_mut(chroma);
        bands.push((first, y_band, u_band, v_band));
        (y_rest, u_rest, v_rest) = (y_next, u_next, v_next);
        first += rows;
    }
    if bands.len() == 1 {
        let (first, y_band, u_band, v_band) = bands.pop().expect("one band");
        scale.band(first, y_band, u_band, v_band);
    } else {
        std::thread::scope(|scope| {
            for (first, y_band, u_band, v_band) in bands {
                let scale = &scale;
                scope.spawn(move || scale.band(first, y_band, u_band, v_band));
            }
        });
    }
    Planes { width, height, y, u, v }
}

/// One scaled conversion, shared by the threads that each do a band of its rows.
struct Scale<'a> {
    src: &'a [u8],
    src_w: usize,
    src_h: usize,
    width: usize,
    height: usize,
    bgra: bool,
    across: usize,
    down: usize,
}

impl Scale<'_> {
    fn sample(&self, row: usize, col: usize) -> (i32, i32, i32) {
        let at = (row * self.src_w + col) * 4;
        match self.src.get(at..at + 3) {
            Some(&[first, g, last]) if self.bgra => (last as i32, g as i32, first as i32),
            Some(&[first, g, last]) => (first as i32, g as i32, last as i32),
            _ => (0, 0, 0),
        }
    }

    /// Rows `first` on, as many as `y` holds, into these slices of the three planes.
    fn band(&self, first: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
        let (width, height, src_w, src_h) = (self.width, self.height, self.src_w, self.src_h);
        let rows = y.len() / width;
        for row in first..first + rows {
            let top = row * src_h / height;
            let bottom = (top + self.down).min(src_h - 1);
            for col in 0..width {
                let left = col * src_w / width;
                let right = (left + self.across).min(src_w - 1);
                let (r, g, b) = if self.across == 0 && self.down == 0 {
                    self.sample(top, left)
                } else {
                    let (a, b, c, d) = (self.sample(top, left), self.sample(top, right), self.sample(bottom, left), self.sample(bottom, right));
                    ((a.0 + b.0 + c.0 + d.0 + 2) / 4, (a.1 + b.1 + c.1 + d.1 + 2) / 4, (a.2 + b.2 + c.2 + d.2 + 2) / 4)
                };
                y[(row - first) * width + col] = clamp(66 * r + 129 * g + 25 * b + 128, 16);
                if row % 2 == 0 && col % 2 == 0 {
                    let chroma = (row / 2 - first / 2) * (width / 2) + col / 2;
                    if let (Some(cu), Some(cv)) = (u.get_mut(chroma), v.get_mut(chroma)) {
                        *cu = clamp(-38 * r - 74 * g + 112 * b + 128, 128);
                        *cv = clamp(112 * r - 94 * g - 18 * b + 128, 128);
                    }
                }
            }
        }
    }
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

/// Where the watched picture is written. The system's temporary directory on a desktop. A phone
/// app may not write there (Android's is `/data/local/tmp`), so it is the app's own data
/// directory, which the shell reading the path shares.
fn picture_dir() -> PathBuf {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        let dir = protocol::support_dir().join("pictures");
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    std::env::temp_dir()
}

/// One picture on disk for the window, and the file before it gone. Decrypted video: only we may
/// read it, and a name someone else made first is not followed.
fn write_jpeg(rgb: &[u8], width: u32, height: u32, generation: u64) -> io::Result<PathBuf> {
    use std::fs::OpenOptions;
    // A local preview and the receive painters each have their own generation counter.
    // Their files must never collide: replacing either image deletes its previous file.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = picture_dir().join(format!("tinyclient-call-{}-{generation}-{unique}.jpg", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Buffered: the encoder writes a few bytes at a time, and each write straight to the file
    // was a system call. That made a 1080p picture cost half a second instead of 15 ms.
    let mut file = io::BufWriter::with_capacity(1 << 16, options.open(&path)?);
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 70);
    let written = encoder.encode(rgb, width, height, image::ExtendedColorType::Rgb8).map_err(io::Error::other).and_then(|()| file.into_inner().map_err(|err| err.into_error())).and_then(|mut file| io::Write::flush(&mut file));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(path)
}

/// One picture on disk for the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
}

/// Whose picture a painted one is: what the window is told with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub user: String,
    pub kind: PictureKind,
}

/// The picture a painter last painted, which is one file at a time.
#[derive(Default)]
struct Canvas {
    /// The file on disk now, removed when the next is written or the picture stops.
    last: Option<PathBuf>,
}

impl Canvas {
    /// Writes the picture as `generation`, which the caller keeps unique: the window tells
    /// pictures apart by it, and so do the files.
    fn paint(&mut self, colors: &[u8], width: u32, height: u32, generation: u64) -> Option<Painted> {
        let path = write_jpeg(colors, width, height, generation).ok()?;
        self.forget();
        self.last = Some(path.clone());
        Some(Painted { path, width, height, generation })
    }

    /// The file on disk goes.
    fn forget(&mut self) {
        if let Some(last) = self.last.take() {
            let _ = std::fs::remove_file(last);
        }
    }
}

/// A call that ends while a picture is on (the capture thread told to stop, with nothing turned
/// off first) leaves no picture of a screen or a face behind in the temp folder.
impl Drop for Canvas {
    fn drop(&mut self) {
        self.forget();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut canvas = Canvas::default();
        let generation = u64::from(std::process::id()) * 1000;
        let colors = [128u8; 2 * 2 * 3];
        let first = canvas.paint(&colors, 2, 2, generation + 1).unwrap();
        assert!(first.path.exists());
        // A frame in between decoded to nothing, so its generation never reached the disk.
        let second = canvas.paint(&colors, 2, 2, generation + 3).unwrap();
        assert!(!first.path.exists(), "the file before the gap went too");
        assert!(second.path.exists());
        canvas.forget();
        assert!(!second.path.exists());
    }

    #[test]
    fn independent_renderers_do_not_delete_each_others_images() {
        let mut local = Canvas::default();
        let mut remote = Canvas::default();
        let colors = [128u8; 2 * 2 * 3];
        let preview = local.paint(&colors, 2, 2, 1).unwrap();
        let received = remote.paint(&colors, 2, 2, 1).unwrap();
        assert_ne!(preview.path, received.path, "generation numbers are local to each renderer");
        local.forget();
        assert!(received.path.exists(), "removing a camera preview must not erase remote video");
        remote.forget();
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
