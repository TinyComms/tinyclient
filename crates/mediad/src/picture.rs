//! One H.264 picture for a Tiny call: a screen or a camera out, and the one picture in.
//!
//! The SFU takes constrained baseline. A share is one layer, at most 1280×720. A camera is two
//! layers, 640×360 and 320×180, sent with the same RTP time. Both run at 15 frames a second.
//! On a desktop (`desktop`) OpenH264 is Cisco's binary, downloaded from Cisco and loaded when the first picture is
//! encoded, so this crate still builds with no C toolchain. A capture that fails is said in
//! words and the voice call stays. A phone captures and codes through its `platform` (`phone`).
//! All of it is slow, and none of it runs on the call's tick: `video` holds the threads.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Said while the binary is on its way, so a call does not treat the wait as a failure.
pub const CODEC_LATER: &str = "OpenH264 is still downloading.";

// Desktop: OpenH264 and the desktop's own camera and screen. Phones: `platform`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use desktop::*;
pub mod convert;
/// Built everywhere, so host tests drive it with `platform::fake`.
pub mod phone;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub use phone::*;

/// A share is fitted inside this. Even, and inside level 3.1.
pub const SHARE_MAX: (u32, u32) = (1280, 720);
/// The camera's high layer.
pub const CAMERA_HIGH: (u32, u32) = (640, 360);
/// The camera's low layer.
pub const CAMERA_LOW: (u32, u32) = (320, 180);

pub const SHARE_BPS: u32 = 1_500_000;
pub const CAMERA_HIGH_BPS: u32 = 500_000;
pub const CAMERA_LOW_BPS: u32 = 150_000;

/// What an encoder is opened at. `video` halves it when encoding falls behind.
const FULL_FPS: f32 = 15.;

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
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Windows, macOS and X11 hand back BGRA. A camera hands back RGBA.
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
    let path = picture_dir().join(format!("tinyclient-call-{}-{generation}.jpg", std::process::id()));
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

/// One picture on disk for the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
}

/// The picture the decoder last painted, which is one file at a time.
#[derive(Default)]
struct Canvas {
    generation: u64,
    /// The file on disk now, removed when the next is written or the picture stops.
    last: Option<PathBuf>,
}

impl Canvas {
    fn paint(&mut self, colors: &[u8], width: u32, height: u32) -> Option<Painted> {
        let generation = self.generation.wrapping_add(1);
        let path = write_jpeg(colors, width, height, generation).ok()?;
        self.generation = generation;
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
        let mut canvas = Canvas { generation: u64::from(std::process::id()) * 1000, ..Canvas::default() };
        let colors = [128u8; 2 * 2 * 3];
        let first = canvas.paint(&colors, 2, 2).unwrap();
        assert!(first.path.exists());
        // A frame in between decoded to nothing, so its generation never reached the disk.
        canvas.generation += 1;
        let second = canvas.paint(&colors, 2, 2).unwrap();
        assert!(!first.path.exists(), "the file before the gap went too");
        assert!(second.path.exists());
        canvas.forget();
        assert!(!second.path.exists());
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
