//! One H.264 picture for a Tiny call: a screen or a camera out, and the one picture in.
//!
//! The SFU takes constrained baseline. A share is one layer, at most 1280×720. A camera is two
//! layers, 640×360 and 320×180, sent with the same RTP time. Both run at 15 frames a second.
//! On a desktop (`desktop`) OpenH264 is Cisco's binary, downloaded from Cisco and loaded when the first picture is
//! encoded, so this crate still builds with no C toolchain. A capture that fails is said in
//! words and the voice call stays. A phone captures and codes through its `platform` (`phone`).

use std::io;
use std::path::PathBuf;

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

pub(crate) const SHARE_BPS: u32 = 1_500_000;
pub(crate) const CAMERA_HIGH_BPS: u32 = 500_000;
pub(crate) const CAMERA_LOW_BPS: u32 = 150_000;

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

/// Writes one JPEG and removes the previous generation, so the window does not keep a stale file.
pub fn write_jpeg(rgb: &[u8], width: u32, height: u32, generation: u64) -> io::Result<PathBuf> {
    let path = picture_dir().join(format!("tinyclient-call-{generation}.jpg"));
    let file = std::fs::File::create(&path)?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 70);
    encoder
        .encode(rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|err| io::Error::other(err))?;
    if generation > 0 {
        let previous = picture_dir().join(format!("tinyclient-call-{}.jpg", generation - 1));
        let _ = std::fs::remove_file(previous);
    }
    Ok(path)
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
