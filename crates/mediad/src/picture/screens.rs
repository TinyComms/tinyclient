//! What a share sends. The share picker lists every screen and, on Windows, every window a person
//! would call one (`--screens`), each with a small JPEG of it; the share then captures the one
//! chosen with `ShareSend`. Windows reads a screen with GDI from the desktop and a window with
//! `PrintWindow`, which draws it even when it is covered. macOS and an X11 session share the
//! primary screen alone, so they list just that. A Wayland session lists the same one row; the
//! picture is the frame a share already recording last produced, and the system's own picker is
//! what chooses the monitor or the window when the share actually starts.

use super::{fit, Rgba};
use protocol::{ScreenSource, SCREENS_LIMIT};
use std::path::Path;
use std::sync::Mutex;

/// The picker's pictures are fitted inside this.
const THUMB: (u32, u32) = (320, 180);
const THUMB_QUALITY: u8 = 70;

/// The screen or window the share captures, by id. `None` is the primary screen. One call per
/// process, so one choice per process.
static CHOSEN: Mutex<Option<String>> = Mutex::new(None);

/// What the share captures from its next frame on.
pub fn choose_screen(id: Option<String>) {
    *CHOSEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = id;
}

pub(super) fn chosen() -> Option<String> {
    CHOSEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Every screen, then every window, at most [`SCREENS_LIMIT`]. With a folder, a picture of each
/// is written there, and the pictures of an earlier list are removed. Each list's files have new
/// names, so a window that caches images by path never shows an old one.
pub fn screens(folder: Option<&Path>) -> Vec<ScreenSource> {
    let mut found = listed();
    found.truncate(SCREENS_LIMIT);
    let Some(folder) = folder else {
        return found;
    };
    if std::fs::create_dir_all(folder).is_err() {
        return found;
    }
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "jpg") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|at| at.as_millis()).unwrap_or_default();
    for (n, source) in found.iter_mut().enumerate() {
        let path = folder.join(format!("screen-{stamp}-{n}.jpg"));
        match super::desktop::capture_source(Some(&source.id)).and_then(|grab| thumbnail(&grab, &path)) {
            Ok(()) => source.thumbnail = path.to_string_lossy().into_owned(),
            Err(err) => tracing::debug!("mediad: no picture of {}: {err}", source.name),
        }
    }
    found
}

/// A small JPEG of one capture, sampled to its nearest pixel: the picker draws it at this size.
pub(super) fn thumbnail(grab: &Rgba, path: &Path) -> Result<(), String> {
    let (width, height) = fit(grab.width, grab.height, THUMB.0, THUMB.1);
    let rgb = shrink(grab, width, height);
    let file = std::fs::File::create(path).map_err(|err| err.to_string())?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), THUMB_QUALITY);
    encoder.encode(&rgb, width, height, image::ExtendedColorType::Rgb8).map_err(|err| err.to_string())
}

/// RGB at `width` × `height`, each pixel the nearest of the capture's.
pub(super) fn shrink(grab: &Rgba, width: u32, height: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        let sy = (y as u64 * grab.height as u64 / height.max(1) as u64) as u32;
        for x in 0..width {
            let sx = (x as u64 * grab.width as u64 / width.max(1) as u64) as u32;
            let at = ((sy * grab.width + sx) * 4) as usize;
            let pixel = grab.pixels.get(at..at + 3).unwrap_or(&[0, 0, 0]);
            if grab.bgra {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            } else {
                rgb.extend_from_slice(pixel);
            }
        }
    }
    rgb
}

#[cfg(not(windows))]
fn listed() -> Vec<ScreenSource> {
    vec![ScreenSource { id: "display:primary".to_owned(), name: "Your screen".to_owned(), primary: true, ..ScreenSource::default() }]
}

#[cfg(windows)]
fn listed() -> Vec<ScreenSource> {
    let _dpi = win::Physical::new();
    let mut found = win::displays()
        .into_iter()
        .enumerate()
        .map(|(n, display)| ScreenSource {
            id: format!("display:{}", display.device),
            name: if display.primary { format!("Screen {} (main)", n + 1) } else { format!("Screen {}", n + 1) },
            primary: display.primary,
            ..ScreenSource::default()
        })
        .collect::<Vec<_>>();
    found.extend(win::windows().into_iter().map(|(handle, title)| ScreenSource { id: format!("window:{handle}"), name: title, window: true, ..ScreenSource::default() }));
    found
}

/// One screen or window, by id; the primary screen when the id is `None` or names nothing that
/// is still there as a screen. A window that closed is said in words, so the share ends.
#[cfg(windows)]
pub(super) fn capture(id: Option<&str>) -> Result<Rgba, String> {
    let _dpi = win::Physical::new();
    if let Some(handle) = id.and_then(|id| id.strip_prefix("window:")).and_then(|handle| handle.parse::<isize>().ok()) {
        return win::capture_window(handle);
    }
    let displays = win::displays();
    let device = id.and_then(|id| id.strip_prefix("display:"));
    let display = displays.iter().find(|display| Some(display.device.as_str()) == device).or_else(|| displays.iter().find(|display| display.primary));
    let Some(display) = display else {
        return Err("The screen has no size.".to_owned());
    };
    match duplicated(&display.device) {
        Ok(frame) => Ok(frame),
        Err(words) => {
            tracing::debug!(words, "the screen fell back to GDI for a frame");
            win::capture_rect(display.rect)
        }
    }
}

/// The screen `device` through Desktop Duplication, kept open from frame to frame; opened
/// again after it was lost. One at a time: the share captures one screen.
#[cfg(windows)]
fn duplicated(device: &str) -> Result<Rgba, String> {
    use super::duplicate::Duplicator;
    static OPEN: Mutex<Option<(String, Duplicator)>> = Mutex::new(None);
    let mut open = OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if open.as_ref().is_none_or(|(name, _)| name != device) {
        *open = Some((device.to_owned(), Duplicator::open(device)?));
    }
    let frame = open.as_mut().expect("just opened").1.frame();
    if frame.is_err() {
        *open = None;
    }
    frame
}

/// How many times a second the shared screen refreshes, when Windows says. A window is on
/// the primary screen as far as this knows.
#[cfg(windows)]
pub(super) fn refresh_rate(id: Option<&str>) -> Option<u32> {
    let device = id.and_then(|id| id.strip_prefix("display:")).map(str::to_owned).or_else(|| win::displays().into_iter().find(|display| display.primary).map(|display| display.device))?;
    win::refresh_rate(&device)
}

#[cfg(windows)]
mod win {
    use super::Rgba;
    use std::mem::size_of;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EnumDisplayMonitors, EnumDisplaySettingsW, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFOHEADER, BI_RGB, DEVMODEW, ENUM_CURRENT_SETTINGS,
        DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, SRCCOPY,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, MONITORINFOF_PRIMARY,
        PW_RENDERFULLCONTENT, WS_EX_TOOLWINDOW,
    };

    /// Real pixels while it lives, on this thread: a screen at 150 % is captured whole and
    /// every rectangle is in the same units, whatever the process's own awareness is.
    pub struct Physical(DPI_AWARENESS_CONTEXT);

    impl Physical {
        pub fn new() -> Physical {
            // SAFETY: sets this thread's awareness and returns the one before, put back on drop.
            Physical(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
        }
    }

    impl Drop for Physical {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the context Windows handed back above.
                unsafe { SetThreadDpiAwarenessContext(self.0) };
            }
        }
    }

    pub struct Display {
        pub device: String,
        pub rect: RECT,
        pub primary: bool,
    }

    /// Every screen, in the order Windows enumerates them.
    pub fn displays() -> Vec<Display> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the Vec below, alive for the whole enumeration.
            let found = unsafe { &mut *(data as *mut Vec<Display>) };
            let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: an EX struct with its size set, as GetMonitorInfoW reads it.
            if unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) } != 0 {
                let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
                found.push(Display {
                    device: String::from_utf16_lossy(&info.szDevice[..len]),
                    rect: info.monitorInfo.rcMonitor,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            1
        }
        let mut found: Vec<Display> = Vec::new();
        // SAFETY: the callback only writes the Vec it is handed.
        unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), (&raw mut found) as LPARAM) };
        found
    }

    /// The windows a person would pick from: shown, titled, not minimized, not a tool window or
    /// another's dialog, and not hidden by the shell (a suspended Store app is "cloaked").
    pub fn windows() -> Vec<(isize, String)> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the Vec below, alive for the whole enumeration.
            let found = unsafe { &mut *(data as *mut Vec<(isize, String)>) };
            if found.len() >= protocol::SCREENS_LIMIT {
                return 0;
            }
            // SAFETY: each of these only reads the window Windows just handed over.
            unsafe {
                if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 || !GetWindow(hwnd, GW_OWNER).is_null() || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0 {
                    return 1;
                }
                let mut cloaked: u32 = 0;
                if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&raw mut cloaked).cast(), size_of::<u32>() as u32) == 0 && cloaked != 0 {
                    return 1;
                }
                let len = GetWindowTextLengthW(hwnd);
                if len <= 0 {
                    return 1;
                }
                let mut title = vec![0u16; len as usize + 1];
                let got = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
                let title = String::from_utf16_lossy(&title[..got.max(0) as usize]);
                // The desktop itself is a titled window too.
                if !title.trim().is_empty() && title != "Program Manager" {
                    found.push((hwnd as isize, title));
                }
            }
            1
        }
        let mut found: Vec<(isize, String)> = Vec::new();
        // SAFETY: the callback only writes the Vec it is handed.
        unsafe { EnumWindows(Some(each), (&raw mut found) as LPARAM) };
        found
    }

    /// The refresh rate of the screen `device` names (`\\.\DISPLAY2`), as its current mode has
    /// it. `None` when Windows does not say, or says 0 or 1, which mean "the default".
    pub fn refresh_rate(device: &str) -> Option<u32> {
        let name: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a zeroed DEVMODEW with its size set, as EnumDisplaySettingsW fills it; the
        // name is NUL terminated and outlives the call.
        unsafe {
            let mut mode: DEVMODEW = std::mem::zeroed();
            mode.dmSize = size_of::<DEVMODEW>() as u16;
            if EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) == 0 {
                return None;
            }
            (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
        }
    }

    /// One rectangle of the desktop, in physical pixels.
    pub fn capture_rect(rect: RECT) -> Result<Rgba, String> {
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if width <= 0 || height <= 0 {
            return Err("The screen has no size.".to_owned());
        }
        // SAFETY: the desktop's DC, released below; the copy lands in a bitmap of that size.
        unsafe {
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err("tinyclient can't record the screen.".to_owned());
            }
            let grabbed = grab(dc, width, height, |memory| BitBlt(memory, 0, 0, width, height, dc, rect.left, rect.top, SRCCOPY) != 0);
            ReleaseDC(std::ptr::null_mut(), dc);
            grabbed.ok_or_else(|| "tinyclient can't record the screen.".to_owned())
        }
    }

    /// One window as it draws itself, even under others.
    pub fn capture_window(handle: isize) -> Result<Rgba, String> {
        let hwnd = handle as HWND;
        const GONE: &str = "The window you shared was closed.";
        // SAFETY: the handle is checked first; a window closing meanwhile only fails the calls.
        unsafe {
            if IsWindow(hwnd) == 0 {
                return Err(GONE.to_owned());
            }
            if IsIconic(hwnd) != 0 {
                return Err("The window you shared is minimized. Restore it to keep sharing.".to_owned());
            }
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rect) == 0 {
                return Err(GONE.to_owned());
            }
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
            if width <= 0 || height <= 0 {
                return Err(GONE.to_owned());
            }
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err("tinyclient can't record the window.".to_owned());
            }
            let grabbed = grab(dc, width, height, |memory| PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT) != 0);
            ReleaseDC(std::ptr::null_mut(), dc);
            grabbed.ok_or_else(|| "tinyclient can't record the window.".to_owned())
        }
    }

    /// A bitmap of `width` × `height` compatible with `dc`, drawn by `draw`, read back as BGRA.
    ///
    /// # Safety
    /// `dc` is a live DC.
    unsafe fn grab(dc: HDC, width: i32, height: i32, draw: impl FnOnce(HDC) -> bool) -> Option<Rgba> {
        unsafe {
            let memory = CreateCompatibleDC(dc);
            let bitmap = CreateCompatibleBitmap(dc, width, height);
            let previous = SelectObject(memory, bitmap as _);
            let drawn = draw(memory);
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
            (drawn && rows != 0).then(|| Rgba { width: width as u32, height: height as u32, pixels, bgra: true })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbnail_keeps_the_shape_and_turns_bgra_round() {
        // 4 × 2, blue-green-red-white across the top as BGRA.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            pixels.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 255, 255, 0]);
        }
        let grab = Rgba { width: 4, height: 2, pixels, bgra: true };
        let rgb = shrink(&grab, 2, 1);
        assert_eq!(rgb, vec![0, 0, 255, 255, 0, 0], "every other pixel, as RGB");
    }

    #[test]
    fn the_choice_is_held_until_changed() {
        choose_screen(Some("display:two".into()));
        assert_eq!(chosen().as_deref(), Some("display:two"));
        choose_screen(None);
        assert_eq!(chosen(), None);
    }
}
