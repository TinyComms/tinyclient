//! Core Video pixel buffers to and from mediad's tightly packed frames.

use mediad::platform::{PixelFormat, VideoFrame};
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Microseconds on the wall clock, for a frame's informational timestamp.
pub(super) fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|since| since.as_micros() as u64).unwrap_or(0)
}

/// Copies `rows` rows of `width` bytes out of a strided plane.
///
/// # Safety
/// `base` points at `rows` rows of `stride` bytes each, `width <= stride`.
unsafe fn copy_plane(out: &mut Vec<u8>, base: *const u8, stride: usize, width: usize, rows: usize) {
    for row in 0..rows {
        // SAFETY: per the function's contract.
        let line = unsafe { std::slice::from_raw_parts(base.add(row * stride), width) };
        out.extend_from_slice(line);
    }
}

/// The picture in `buffer`, tightly packed: NV12 for the biplanar YUV formats, BGRA for
/// 32BGRA. Odd sizes are cropped to even ones. `None` for any other format.
pub(super) fn frame_of(buffer: &CVPixelBuffer) -> Option<VideoFrame> {
    // SAFETY: the buffer is locked for reading while its planes are copied, and every pointer
    // and stride comes from Core Video for that buffer.
    unsafe {
        let format = CVPixelBufferGetPixelFormatType(buffer);
        let width = CVPixelBufferGetWidth(buffer) & !1;
        let height = CVPixelBufferGetHeight(buffer) & !1;
        if width == 0 || height == 0 {
            return None;
        }
        if CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return None;
        }
        let frame = if format == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            || format == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        {
            let mut data = Vec::with_capacity(width * height * 3 / 2);
            let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0) as *const u8;
            let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1) as *const u8;
            if luma.is_null() || chroma.is_null() {
                None
            } else {
                copy_plane(&mut data, luma, CVPixelBufferGetBytesPerRowOfPlane(buffer, 0), width, height);
                copy_plane(&mut data, chroma, CVPixelBufferGetBytesPerRowOfPlane(buffer, 1), width, height / 2);
                Some((PixelFormat::Nv12, data))
            }
        } else if format == kCVPixelFormatType_32BGRA {
            let mut data = Vec::with_capacity(width * height * 4);
            let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
            if base.is_null() {
                None
            } else {
                copy_plane(&mut data, base, CVPixelBufferGetBytesPerRow(buffer), width * 4, height);
                Some((PixelFormat::Bgra, data))
            }
        } else {
            None
        };
        CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        let (format, data) = frame?;
        Some(VideoFrame { width: width as u32, height: height as u32, format, data, timestamp_us: now_us() })
    }
}

/// Writes an NV12 `frame` into a biplanar `buffer` of the same size.
pub(super) fn fill_nv12(buffer: &CVPixelBuffer, frame: &VideoFrame) -> Result<(), String> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    if frame.format != PixelFormat::Nv12 || !frame.is_whole() {
        return Err("The encoder was given a frame that is not whole NV12.".to_owned());
    }
    // SAFETY: the buffer is locked for writing; rows are copied within the strides Core Video
    // reports, and the frame is whole (checked above).
    unsafe {
        if CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags(0)) != 0 {
            return Err("The encoder's picture buffer could not be locked.".to_owned());
        }
        let planes = [(0usize, 0usize, height), (1, width * height, height / 2)];
        for (plane, offset, rows) in planes {
            let base = CVPixelBufferGetBaseAddressOfPlane(buffer, plane) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
            if base.is_null() || stride < width {
                CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags(0));
                return Err("The encoder's picture buffer has an unexpected layout.".to_owned());
            }
            for row in 0..rows {
                let from = &frame.data[offset + row * width..offset + (row + 1) * width];
                std::ptr::copy_nonoverlapping(from.as_ptr(), base.add(row * stride), width);
            }
        }
        CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags(0));
    }
    Ok(())
}
