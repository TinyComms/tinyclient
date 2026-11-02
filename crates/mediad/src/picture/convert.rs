//! Between the platform's frames (`platform::VideoFrame`) and the I420 planes the rest of
//! `picture` works in.

use super::{i420, Planes};
use crate::platform::{PixelFormat, VideoFrame};

/// `frame` scaled to `width` × `height` (even) as I420. Nearest neighbour, as `i420` scales.
pub fn planes(frame: &VideoFrame, width: u32, height: u32) -> Planes {
    match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => {
            i420(&frame.data, frame.width, frame.height, width, height, frame.format == PixelFormat::Bgra)
        }
        PixelFormat::I420 | PixelFormat::Nv12 => yuv(frame, width, height),
    }
}

fn yuv(frame: &VideoFrame, width: u32, height: u32) -> Planes {
    let (w, h) = (width.max(2) as usize & !1, height.max(2) as usize & !1);
    let (sw, sh) = (frame.width.max(2) as usize, frame.height.max(2) as usize);
    let luma = sw * sh;
    let chroma_w = sw / 2;
    let byte = |at: usize| frame.data.get(at).copied().unwrap_or(128);
    let mut y = vec![0u8; w * h];
    for row in 0..h {
        let sy = row * sh / h;
        for col in 0..w {
            y[row * w + col] = frame.data.get(sy * sw + col * sw / w).copied().unwrap_or(16);
        }
    }
    let (cw, ch) = (w / 2, h / 2);
    let (mut u, mut v) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
    for row in 0..ch {
        let sy = row * (sh / 2) / ch;
        for col in 0..cw {
            let sx = col * chroma_w / cw;
            let (cb, cr) = match frame.format {
                PixelFormat::Nv12 => (byte(luma + (sy * chroma_w + sx) * 2), byte(luma + (sy * chroma_w + sx) * 2 + 1)),
                _ => (byte(luma + sy * chroma_w + sx), byte(luma + luma / 4 + sy * chroma_w + sx)),
            };
            u[row * cw + col] = cb;
            v[row * cw + col] = cr;
        }
    }
    Planes { width: w, height: h, y, u, v }
}

/// The planes as one NV12 frame, what a platform encoder takes.
pub fn nv12(planes: &Planes) -> VideoFrame {
    let mut data = Vec::with_capacity(planes.y.len() + planes.u.len() * 2);
    data.extend_from_slice(&planes.y);
    for (cb, cr) in planes.u.iter().zip(&planes.v) {
        data.push(*cb);
        data.push(*cr);
    }
    VideoFrame { width: planes.width as u32, height: planes.height as u32, format: PixelFormat::Nv12, data, timestamp_us: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_and_i420_come_back_as_the_same_planes() {
        let frame = crate::platform::fake::stripes(64, 32, 3);
        let planes = planes(&frame, 64, 32);
        assert_eq!(nv12(&planes), VideoFrame { timestamp_us: 0, ..frame.clone() });
        let mut i420 = planes.y.clone();
        i420.extend_from_slice(&planes.u);
        i420.extend_from_slice(&planes.v);
        let as_i420 = VideoFrame { width: 64, height: 32, format: PixelFormat::I420, data: i420, timestamp_us: 0 };
        let again = super::planes(&as_i420, 64, 32);
        assert_eq!((again.y, again.u, again.v), (planes.y, planes.u, planes.v));
    }

    #[test]
    fn a_frame_is_scaled_to_the_layer_and_kept_even() {
        let frame = crate::platform::fake::stripes(640, 360, 0);
        let low = planes(&frame, 320, 180);
        assert_eq!((low.width, low.height, low.y.len(), low.u.len()), (320, 180, 320 * 180, 160 * 90));
    }
}
