//! The window's taskbar button (Windows) or Dock icon (macOS): a flash or bounce when something
//! notifies while the window is in the background (N10), and the number of what needs me
//! (N11). Called by the window process, on its main thread, since both belong to its window.

/// The badge's words: nothing for 0, the number, or "99+".
pub fn badge_words(count: u32) -> Option<String> {
    match count {
        0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".to_owned()),
    }
}

#[cfg(windows)]
pub use self::windows::{badge, flash};

#[cfg(target_os = "macos")]
pub use self::macos::{badge, flash};

#[cfg(windows)]
mod windows {
    use super::badge_words;
    use std::cell::Cell;
    use windows::core::{BOOL, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, SelectObject,
        SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_BOLD, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, EnumWindows, FlashWindowEx, GetWindow, GetWindowThreadProcessId, IsWindowVisible, FLASHWINFO,
        FLASHW_TIMERNOFG, FLASHW_TRAY, GW_OWNER, HICON, ICONINFO,
    };

    /// The overlay is drawn at 32 px and scaled by the taskbar; 16 is what it shows at 100 %.
    const SIZE: i32 = 32;

    thread_local! {
        /// The icon on the button now, destroyed when the next replaces it.
        static SHOWN: Cell<Option<(u32, isize)>> = const { Cell::new(None) };
    }

    /// This process's main window: top level, visible, owned by nobody.
    fn own_window() -> Option<HWND> {
        unsafe extern "system" fn each(window: HWND, found: LPARAM) -> BOOL {
            let mut pid = 0u32;
            // SAFETY: reading the window's owner process and state; `found` points at the
            // `Option<HWND>` below for the length of EnumWindows.
            unsafe {
                GetWindowThreadProcessId(window, Some(&mut pid));
                let ours = pid == std::process::id() && IsWindowVisible(window).as_bool() && GetWindow(window, GW_OWNER).is_err();
                if ours {
                    *(found.0 as *mut Option<HWND>) = Some(window);
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
        let mut found: Option<HWND> = None;
        // SAFETY: the callback only writes `found`, which outlives the call.
        let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut found as *mut Option<HWND> as isize)) };
        found
    }

    /// Flashes the taskbar button until the window comes to the front.
    pub fn flash() {
        let Some(window) = own_window() else { return };
        let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: window, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
        // SAFETY: a plain call with a filled-in struct.
        unsafe {
            let _ = FlashWindowEx(&info);
        }
    }

    /// A red disc with the number in white, premultiplied BGRA, as an icon.
    fn icon(words: &str) -> Option<HICON> {
        // SAFETY: GDI objects made here are selected out and deleted before returning; the DIB's
        // pixels are written only while the bitmap lives.
        unsafe {
            let header = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE,
                biHeight: -SIZE,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
            let mut bits = std::ptr::null_mut();
            let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let pixels = std::slice::from_raw_parts_mut(bits as *mut [u8; 4], (SIZE * SIZE) as usize);
            // The disc, with a one-pixel soft edge.
            let centre = (SIZE as f32 - 1.) / 2.;
            let radius = SIZE as f32 / 2.;
            for (at, pixel) in pixels.iter_mut().enumerate() {
                let (x, y) = ((at as i32 % SIZE) as f32, (at as i32 / SIZE) as f32);
                let distance = ((x - centre).powi(2) + (y - centre).powi(2)).sqrt();
                let cover = (radius - distance).clamp(0., 1.);
                let a = (cover * 255.) as u8;
                // #e5484d, premultiplied.
                *pixel = [(0x4d as f32 * cover) as u8, (0x48 as f32 * cover) as u8, (0xe5 as f32 * cover) as u8, a];
            }
            let dc = CreateCompatibleDC(None);
            let before = SelectObject(dc, HGDIOBJ(color.0));
            let height = if words.len() > 2 { 15 } else if words.len() == 2 { 19 } else { 22 };
            let font = CreateFontW(-height, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, windows::core::w!("Segoe UI"));
            let old_font = SelectObject(dc, HGDIOBJ(font.0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00ff_ffff));
            let mut text: Vec<u16> = words.encode_utf16().collect();
            let mut rect = RECT { left: 0, top: 0, right: SIZE, bottom: SIZE };
            DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            let _ = GdiFlush();
            // GDI zeroes the alpha of every pixel it draws. The text sits inside the disc,
            // which is opaque there, so a coloured pixel with no alpha is text: opaque again.
            for pixel in pixels.iter_mut() {
                if pixel[3] == 0 && (pixel[0] | pixel[1] | pixel[2]) != 0 {
                    pixel[3] = 255;
                }
            }
            SelectObject(dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            SelectObject(dc, before);
            let _ = DeleteDC(dc);
            let mask = CreateBitmap(SIZE, SIZE, 1, 1, None);
            let made = CreateIconIndirect(&ICONINFO { fIcon: BOOL(1), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color });
            let _ = DeleteObject(HGDIOBJ(mask.0));
            let _ = DeleteObject(HGDIOBJ(color.0));
            made.ok()
        }
    }

    /// The number on the taskbar button, or none for 0.
    pub fn badge(count: u32) {
        let words = badge_words(count);
        if SHOWN.get().map(|(shown, _)| shown) == Some(count) {
            return;
        }
        let Some(window) = own_window() else { return };
        // SAFETY: COM on the window's own thread, which GPUI has already initialised; the
        // icon handed to the taskbar is kept until the next one replaces it.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let Ok(taskbar) = CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else { return };
            if taskbar.HrInit().is_err() {
                return;
            }
            let (made, description) = match &words {
                Some(words) => (icon(words), format!("{words} unread\0")),
                None => (None, "\0".to_owned()),
            };
            let description: Vec<u16> = description.encode_utf16().collect();
            let set = taskbar.SetOverlayIcon(window, made.unwrap_or_default(), PCWSTR(description.as_ptr()));
            if set.is_ok() {
                if let Some((_, old)) = SHOWN.replace(Some((count, made.map_or(0, |icon| icon.0 as isize)))) {
                    if old != 0 {
                        let _ = DestroyIcon(HICON(old as *mut core::ffi::c_void));
                    }
                }
            } else if let Some(icon) = made {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::badge_words;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
    use objc2_foundation::NSString;

    /// Bounces the Dock icon once, if the app is not active.
    pub fn flash() {
        let Some(main) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(main);
        let _ = app.requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    }

    /// The number on the Dock icon, or none for 0.
    pub fn badge(count: u32) {
        let Some(main) = MainThreadMarker::new() else { return };
        let tile = NSApplication::sharedApplication(main).dockTile();
        let words = badge_words(count).map(|words| NSString::from_str(&words));
        tile.setBadgeLabel(words.as_deref());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_badge_counts_to_ninety_nine() {
        assert_eq!(super::badge_words(0), None);
        assert_eq!(super::badge_words(7).as_deref(), Some("7"));
        assert_eq!(super::badge_words(100).as_deref(), Some("99+"));
    }
}
