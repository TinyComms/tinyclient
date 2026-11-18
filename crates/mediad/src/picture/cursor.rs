//! The sharer's pointer drawn into a share's frames (`#797`). Windows leaves the pointer out of
//! a GDI grab and a Desktop Duplication frame alike, so it is read where it is (`GetCursorInfo`),
//! its shape decoded once and kept (`GetIconInfo`), and only the pointer's own rectangle drawn:
//! GDI draws it onto the memory DC before the read-back, a duplication frame blends it in after.
//! A pointer Windows says is hidden (a game drew its own) or suppressed (a touch screen is in
//! use) draws nothing, and so does one off the frame. The thread is per-monitor DPI aware when
//! this runs (`screens::capture`), so the pointer's place is in real pixels, like the frame's.
//! Off is [`super::cursor_shown`].

use super::Rgba;
use std::mem::size_of;
use std::sync::Mutex;
use windows_sys::Win32::Graphics::Gdi::{DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC};
use windows_sys::Win32::UI::WindowsAndMessaging::{DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, CURSOR_SUPPRESSED, DI_NORMAL, HCURSOR, ICONINFO};

/// The pointer's shape: its hotspot and its BGRA rows, top down, straight alpha as Windows
/// keeps it for a cursor's bitmap.
#[derive(Clone)]
struct Pointer {
    hot_x: i32,
    hot_y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// The shape decoded last, kept by cursor handle: the shape changes when the system's pointer
/// does, which is rare, and decoding reads two bitmaps. Only the capture thread reads this.
static DECODED: Mutex<Option<(usize, Pointer)>> = Mutex::new(None);

/// The pointer as it is now, over the rectangle (`left`, `top`, `right`, `bottom`): where its
/// top-left goes in a frame of that rectangle, its handle, and its shape. `None` when the share
/// hides it, it is hidden or suppressed, it is off the rectangle, or its shape cannot be read.
fn now(left: i32, top: i32, right: i32, bottom: i32) -> Option<(i32, i32, HCURSOR, Pointer)> {
    if !super::cursor_shown() {
        return None;
    }
    let mut info: CURSORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<CURSORINFO>() as u32;
    // SAFETY: a zeroed CURSORINFO with its size set, as GetCursorInfo reads it.
    if unsafe { GetCursorInfo(&mut info) } == 0 || !visible(info.flags) {
        return None;
    }
    let pointer = decoded(info.hCursor)?;
    let (x, y) = placed((info.ptScreenPos.x, info.ptScreenPos.y), (pointer.hot_x, pointer.hot_y), (left, top, right, bottom), (pointer.width, pointer.height))?;
    Some((x, y, info.hCursor, pointer))
}

/// Draws the pointer onto the memory DC a GDI grab drew the desktop rectangle into, before the
/// read-back: no second copy of the frame.
pub(super) fn draw_on(dc: HDC, left: i32, top: i32, right: i32, bottom: i32) {
    let Some((x, y, handle, _)) = now(left, top, right, bottom) else { return };
    // SAFETY: `dc` is the live memory DC the grab drew into; the handle is the system's cursor.
    unsafe { DrawIconEx(dc, x, y, handle, 0, 0, 0, std::ptr::null_mut(), DI_NORMAL) };
}

/// Blends the pointer into a BGRA frame of the desktop rectangle at (`left`, `top`), which a
/// Desktop Duplication frame hands back as plain bytes. Only the pointer's own rows go in.
pub(super) fn blend(frame: &mut Rgba, left: i32, top: i32) {
    let (width, height) = (frame.width as i32, frame.height as i32);
    let Some((x, y, _, pointer)) = now(left, top, left + width, top + height) else { return };
    blend_at(&mut frame.pixels, frame.width, frame.height, x, y, &pointer);
}

/// A pointer Windows says is showing draws; hidden (a game drew its own) and suppressed (a
/// touch screen is in use) do not.
fn visible(flags: u32) -> bool {
    flags & CURSOR_SHOWING != 0 && flags & CURSOR_SUPPRESSED == 0
}

/// Where the pointer's top-left sits in a frame of the rectangle (`left`, `top`, `right`,
/// `bottom`), its hotspot subtracted. `None` when the whole of it is off the rectangle; part
/// over an edge is a place, and the blend clips it.
fn placed(at: (i32, i32), hot: (i32, i32), bounds: (i32, i32, i32, i32), size: (u32, u32)) -> Option<(i32, i32)> {
    let (x, y) = (at.0 - bounds.0 - hot.0, at.1 - bounds.1 - hot.1);
    (x + size.0 as i32 > 0 && y + size.1 as i32 > 0 && x < bounds.2 - bounds.0 && y < bounds.3 - bounds.1).then_some((x, y))
}

/// The pointer's pixels over the frame's, with its top-left at `x`, `y`, which may hang over an
/// edge: only the part on the frame goes in. Straight alpha over, so a translucent edge keeps
/// the screen behind it.
fn blend_at(pixels: &mut [u8], width: u32, height: u32, x: i32, y: i32, pointer: &Pointer) {
    for row in 0..pointer.height as usize {
        let frame_row = y + row as i32;
        if frame_row < 0 || frame_row >= height as i32 {
            continue;
        }
        for col in 0..pointer.width as usize {
            let frame_col = x + col as i32;
            if frame_col < 0 || frame_col >= width as i32 {
                continue;
            }
            let from = (row * pointer.width as usize + col) * 4;
            let Some(&[b, g, r, a]) = pointer.pixels.get(from..from + 4) else { continue };
            if a == 0 {
                continue;
            }
            let at = ((frame_row as u32 * width + frame_col as u32) * 4) as usize;
            let Some(to) = pixels.get_mut(at..at + 4) else { continue };
            let over = |front: u8, behind: u8| ((front as u32 * a as u32 + behind as u32 * (255 - a as u32)) / 255) as u8;
            to[0] = over(b, to[0]);
            to[1] = over(g, to[1]);
            to[2] = over(r, to[2]);
            to[3] = 255;
        }
    }
}

/// The shape of the cursor `handle`, as it was decoded last or decoded now.
fn decoded(handle: HCURSOR) -> Option<Pointer> {
    let mut kept = DECODED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if kept.as_ref().is_none_or(|(seen, _)| *seen != handle as usize) {
        *kept = decode(handle).map(|pointer| (handle as usize, pointer));
    }
    kept.as_ref().map(|(_, pointer)| pointer.clone())
}

/// Reads the cursor's bitmaps and flattens them to BGRA with its hotspot. The three shapes
/// Windows keeps: colour with an alpha channel, colour under a 1-bit mask, and the old
/// monochrome pair.
fn decode(handle: HCURSOR) -> Option<Pointer> {
    let mut info: ICONINFO = unsafe { std::mem::zeroed() };
    // SAFETY: a zeroed ICONINFO as GetIconInfo fills it.
    if unsafe { GetIconInfo(handle, &mut info) } == 0 {
        return None;
    }
    let pointer = shape(&info);
    // SAFETY: the bitmaps GetIconInfo handed back are ours to delete; deleting twice would be
    // a bug, so once here.
    unsafe {
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask as _);
        }
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor as _);
        }
    }
    pointer
}

/// The colour bitmap decides: with an alpha anywhere it is the modern shape, without one the
/// old masked shape under the mask, and with no colour bitmap at all the monochrome pair.
fn shape(info: &ICONINFO) -> Option<Pointer> {
    let Some((width, height)) = bitmap(info.hbmColor) else {
        return mono(info.hbmMask, info.xHotspot as i32, info.yHotspot as i32);
    };
    let pixels = read(info.hbmColor, width, height, stride(width, 32), 32)?;
    let mut pointer = Pointer { hot_x: info.xHotspot as i32, hot_y: info.yHotspot as i32, width, height, pixels };
    if pointer.pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
        return Some(pointer);
    }
    keep_visible(&mut pointer, info.hbmMask)?;
    Some(pointer)
}

/// The old masked shape: a 1-bit mask under the colour, a set bit hiding the pixel. Where a
/// set bit inverted the screen instead, which is what it used to mean, the pixel is hidden:
/// no system cursor draws that any more.
fn keep_visible(pointer: &mut Pointer, mask: HBITMAP) -> Option<()> {
    let (width, height) = bitmap(mask)?;
    if (width, height) != (pointer.width, pointer.height) {
        return None;
    }
    let masks = read(mask, width, height, stride(width, 1), 1)?;
    for (at, pixel) in pointer.pixels.chunks_exact_mut(4).enumerate() {
        if bit(&masks, at / width as usize, at % width as usize, stride(width, 1) as usize) {
            pixel[3] = 0;
        }
    }
    Some(())
}

/// The old monochrome pair: one bitmap of two halves, the AND mask on top and the XOR below.
/// A pixel is black, white, off, or the screen inverted, which is drawn as white, as no
/// system cursor draws that any more.
fn mono(mask: HBITMAP, hot_x: i32, hot_y: i32) -> Option<Pointer> {
    let (width, tall) = bitmap(mask)?;
    if tall % 2 != 0 || tall == 0 {
        return None;
    }
    let masks = read(mask, width, tall, stride(width, 1), 1)?;
    let height = tall / 2;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for row in 0..height as usize {
        for col in 0..width as usize {
            let (hide, white) = (bit(&masks, row, col, stride(width, 1) as usize), bit(&masks, height as usize + row, col, stride(width, 1) as usize));
            let at = (row * width as usize + col) * 4;
            pixels[at..at + 4].copy_from_slice(match (hide, white) {
                (true, false) => [0, 0, 0, 0],
                (false, false) => [0, 0, 0, 255],
                (_, true) => [255, 255, 255, 255],
            });
        }
    }
    Some(Pointer { hot_x, hot_y, width, height, pixels })
}

/// One bit of a 1-bit DIB row, most significant pixel first.
fn bit(bytes: &[u8], row: usize, col: usize, stride: usize) -> bool {
    bytes.get(row * stride + col / 8).copied().unwrap_or(0) & (0x80 >> (col % 8)) != 0
}

/// A bitmap's width and height, as Windows says.
fn bitmap(handle: HBITMAP) -> Option<(u32, u32)> {
    let mut info: BITMAP = unsafe { std::mem::zeroed() };
    // SAFETY: a zeroed BITMAP as GetObjectW fills it, of the size GetObjectW expects.
    if unsafe { GetObjectW(handle as _, size_of::<BITMAP>() as i32, (&raw mut info).cast()) } == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
        return None;
    }
    Some((info.bmWidth as u32, info.bmHeight as u32))
}

/// A DIB row's bytes at `bits` a pixel: rows align on four, whatever the bitmap keeps.
fn stride(width: u32, bits: u32) -> u32 {
    (width * bits + 31) / 32 * 4
}

/// A bitmap's rows, top down, `bits` a pixel (32 colour, 1 the masks), each row `stride`.
fn read(handle: HBITMAP, width: u32, height: u32, stride: u32, bits: u32) -> Option<Vec<u8>> {
    // SAFETY: a null window asks for the screen's DC; the call only hands out a handle or
    // null, checked below.
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return None;
    }
    let mut header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: bits as u16,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let mut out = vec![0u8; stride as usize * height as usize];
    // SAFETY: the desktop's DC, released below; the read lands in the rows and stride Windows
    // reported, in a buffer of exactly that size.
    let rows = unsafe { GetDIBits(dc, handle, 0, height, out.as_mut_ptr().cast(), (&mut header as *mut BITMAPINFOHEADER).cast(), DIB_RGB_COLORS) };
    // SAFETY: the screen DC GetDC handed back above, released once with the matching null
    // window, on the thread that took it; nothing reads it after.
    unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    (rows == height as i32).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_windows_hides_draws_nothing() {
        assert!(visible(CURSOR_SHOWING));
        assert!(!visible(0), "a game drew its own pointer");
        assert!(!visible(CURSOR_SHOWING | CURSOR_SUPPRESSED), "a touch screen suppressed it");
    }

    #[test]
    fn a_pointer_off_the_frame_draws_nothing_and_one_over_an_edge_keeps_its_place() {
        let size = (8, 8);
        assert_eq!(placed((100, 100), (0, 0), (0, 0, 60, 40), size), None);
        assert_eq!(placed((20, 10), (0, 0), (0, 0, 60, 40), size), Some((20, 10)));
        // The hotspot moves it back, and part over an edge is still a place.
        assert_eq!(placed((24, 36), (2, 4), (0, 0, 60, 40), size), Some((22, 32)));
        assert_eq!(placed((12, 12), (4, 4), (10, 10, 60, 40), size), Some((-2, -2)));
        assert_eq!(placed((0, 0), (4, 4), (10, 10, 60, 40), size), None, "wholly off the top left");
    }

    #[test]
    fn an_opaque_pointer_pixel_replaces_the_frame_and_a_transparent_one_leaves_it() {
        let mut frame = vec![10, 20, 30, 255; 2 * 2 * 4];
        let pointer = Pointer { hot_x: 0, hot_y: 0, width: 2, height: 1, pixels: vec![90, 100, 110, 255, 90, 100, 110, 0] };
        blend_at(&mut frame, 2, 2, 0, 0, &pointer);
        assert_eq!(frame[0..8], [90, 100, 110, 255, 10, 20, 30, 255]);
    }

    #[test]
    fn a_see_through_pointer_pixel_blends_the_screen_behind_it() {
        let mut frame = vec![40, 40, 40, 255; 2 * 2 * 4];
        // Half alpha white: half way from the grey behind to white, in the frame's BGRA order.
        let pointer = Pointer { hot_x: 0, hot_y: 0, width: 1, height: 1, pixels: vec![0, 0, 255, 128] };
        blend_at(&mut frame, 2, 2, 0, 0, &pointer);
        assert_eq!(frame[0..4], [19, 19, 147, 255]);
    }

    #[test]
    fn a_pointer_over_an_edge_draws_only_the_part_on_the_frame() {
        let mut frame = vec![0u8; 4 * 2 * 4];
        let pixels = vec![255u8, 255, 255, 255].repeat(4);
        let pointer = Pointer { hot_x: 0, hot_y: 0, width: 2, height: 2, pixels };
        blend_at(&mut frame, 4, 2, 3, 1, &pointer);
        assert_eq!(frame.iter().filter(|&&byte| byte == 255).count(), 4, "one pixel of four is on the frame");
    }
}
