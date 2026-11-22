use super::Rgba;
use memmap2::MmapRaw;
use protocol::ScreenSource;
use std::sync::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::{composite, randr, shm, xfixes, xproto::*};
use x11rb::rust_connection::RustConnection;

const GONE: &str = "The shared window closed or is no longer visible.";
const NO_DISPLAY: &str = "This session has no X11 display, so the screen can't be shared.";
const READ_FAILED: &str = "The selected X11 source could not be read.";

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_WM_NAME,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DOCK,
        UTF8_STRING,
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

impl Rect {
    fn clip(self, width: u16, height: u16) -> Option<Self> {
        let left = i32::from(self.x).max(0);
        let top = i32::from(self.y).max(0);
        let right = (i32::from(self.x) + i32::from(self.width)).min(i32::from(width));
        let bottom = (i32::from(self.y) + i32::from(self.height)).min(i32::from(height));
        (right > left && bottom > top).then(|| Self {
            x: left as i16,
            y: top as i16,
            width: (right - left) as u16,
            height: (bottom - top) as u16,
        })
    }
}

struct Monitor {
    id: String,
    name: String,
    primary: bool,
    rect: Rect,
}

struct SharedImage {
    segment: shm::Seg,
    map: MmapRaw,
}

struct Capture {
    conn: RustConnection,
    root: Window,
    randr: bool,
    composite: bool,
    fixes: bool,
    shm: bool,
    shared: Option<SharedImage>,
}

static LIVE: Mutex<Option<Capture>> = Mutex::new(None);

pub(super) fn stop() {
    LIVE.lock().unwrap_or_else(|err| err.into_inner()).take();
}

pub(super) fn listed() -> Vec<ScreenSource> {
    let Ok(capture) = Capture::open() else {
        return Vec::new();
    };
    let mut sources: Vec<_> = capture
        .monitors()
        .into_iter()
        .map(|monitor| ScreenSource {
            id: format!("display:{}", monitor.id),
            name: monitor.name,
            primary: monitor.primary,
            ..ScreenSource::default()
        })
        .collect();
    sources.extend(capture.windows());
    sources
}

pub(super) fn capture(id: Option<&str>, live: bool) -> Result<Rgba, String> {
    if !live {
        return Capture::open()?.frame(id, false);
    }
    let mut capture = LIVE.lock().unwrap_or_else(|err| err.into_inner());
    if capture.is_none() {
        *capture = Some(Capture::open()?);
    }
    let frame = capture.as_mut().unwrap().frame(id, super::cursor_shown());
    if frame.is_err() {
        *capture = None;
    }
    frame
}

impl Capture {
    fn open() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|_| NO_DISPLAY.to_owned())?;
        let root = conn.setup().roots[screen].root;
        let randr = randr::query_version(&conn, 1, 5)
            .ok()
            .and_then(|reply| reply.reply().ok())
            .is_some_and(|reply| (reply.major_version, reply.minor_version) >= (1, 5));
        let composite = composite::query_version(&conn, 0, 4)
            .ok()
            .and_then(|reply| reply.reply().ok())
            .is_some_and(|reply| (reply.major_version, reply.minor_version) >= (0, 2));
        let fixes = xfixes::query_version(&conn, 2, 0)
            .ok()
            .and_then(|reply| reply.reply().ok())
            .is_some_and(|reply| reply.major_version >= 2);
        // FD-backed MIT-SHM avoids SysV segment permissions and works on local UNIX sockets.
        let shm = shm::query_version(&conn)
            .ok()
            .and_then(|reply| reply.reply().ok())
            .is_some_and(|reply| (reply.major_version, reply.minor_version) >= (1, 2));
        Ok(Self {
            conn,
            root,
            randr,
            composite,
            fixes,
            shm,
            shared: None,
        })
    }

    fn monitors(&self) -> Vec<Monitor> {
        if self.randr {
            if let Some(reply) = randr::get_monitors(&self.conn, self.root, true)
                .ok()
                .and_then(|reply| reply.reply().ok())
            {
                let has_primary = reply.monitors.iter().any(|monitor| monitor.primary);
                let mut monitors = Vec::new();
                for (index, monitor) in reply.monitors.into_iter().enumerate() {
                    let primary = monitor.primary || (!has_primary && index == 0);
                    let name = self
                        .conn
                        .get_atom_name(monitor.name)
                        .ok()
                        .and_then(|reply| reply.reply().ok())
                        .map(|reply| String::from_utf8_lossy(&reply.name).into_owned());
                    let label = if primary { " (main)" } else { "" };
                    monitors.push(Monitor {
                        id: monitor.name.to_string(),
                        name: name.map_or_else(
                            || format!("Screen {}{label}", index + 1),
                            |name| format!("{name}{label}"),
                        ),
                        primary,
                        rect: Rect {
                            x: monitor.x,
                            y: monitor.y,
                            width: monitor.width,
                            height: monitor.height,
                        },
                    });
                }
                if !monitors.is_empty() {
                    monitors.sort_by_key(|monitor| !monitor.primary);
                    return monitors;
                }
            }
        }
        self.root_rect()
            .map(|rect| {
                vec![Monitor {
                    id: "primary".into(),
                    name: "Your screen".into(),
                    primary: true,
                    rect,
                }]
            })
            .unwrap_or_default()
    }

    fn root_rect(&self) -> Option<Rect> {
        let root = self.conn.get_geometry(self.root).ok()?.reply().ok()?;
        Some(Rect {
            x: 0,
            y: 0,
            width: root.width,
            height: root.height,
        })
    }

    fn windows(&self) -> Vec<ScreenSource> {
        let Some(atoms) = Atoms::new(&self.conn)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return Vec::new();
        };
        let Some(clients) =
            self.property(self.root, atoms._NET_CLIENT_LIST, AtomEnum::WINDOW.into())
        else {
            return Vec::new();
        };
        clients
            .value32()
            .into_iter()
            .flatten()
            .filter_map(|window| {
                if let Some(kind) =
                    self.property(window, atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM.into())
                {
                    if kind.value32().into_iter().flatten().any(|kind| {
                        kind == atoms._NET_WM_WINDOW_TYPE_DOCK
                            || kind == atoms._NET_WM_WINDOW_TYPE_DESKTOP
                    }) {
                        return None;
                    }
                }
                let class =
                    self.property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
                if class.as_ref().is_some_and(|class| {
                    class.value.split(|byte| *byte == 0).any(|s| {
                        s.eq_ignore_ascii_case(b"dev.tinyclient.app")
                            || s.eq_ignore_ascii_case(b"tinyclient")
                    })
                }) {
                    return None;
                }
                if self
                    .conn
                    .get_window_attributes(window)
                    .ok()?
                    .reply()
                    .ok()?
                    .map_state
                    != MapState::VIEWABLE
                {
                    return None;
                }
                let name = self
                    .property(window, atoms._NET_WM_NAME, atoms.UTF8_STRING)
                    .or_else(|| {
                        self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::ANY.into())
                    })?;
                let name = String::from_utf8_lossy(&name.value)
                    .trim_matches('\0')
                    .trim()
                    .to_owned();
                (!name.is_empty()).then(|| ScreenSource {
                    id: format!("window:{window}"),
                    name,
                    window: true,
                    ..ScreenSource::default()
                })
            })
            .collect()
    }

    fn property(&self, window: Window, name: Atom, kind: Atom) -> Option<GetPropertyReply> {
        let reply = self
            .conn
            .get_property(false, window, name, kind, 0, 8192)
            .ok()?
            .reply()
            .ok()?;
        (reply.format != 0).then_some(reply)
    }

    fn frame(&mut self, id: Option<&str>, cursor: bool) -> Result<Rgba, String> {
        let (drawable, rect, origin, pixmap) =
            if let Some(id) = id.and_then(|id| id.strip_prefix("window:")) {
                let window = id.parse::<Window>().map_err(|_| GONE.to_owned())?;
                let (rect, origin) = self.window_rect(window).ok_or_else(|| GONE.to_owned())?;
                if let Some((pixmap, pixels)) = self.window_pixmap(window) {
                    (pixmap, pixels, origin, Some(pixmap))
                } else {
                    let root = self.root_rect().ok_or_else(|| READ_FAILED.to_owned())?;
                    let rect = rect
                        .clip(root.width, root.height)
                        .ok_or_else(|| GONE.to_owned())?;
                    (self.root, rect, (rect.x, rect.y), None)
                }
            } else {
                let monitor = self
                    .monitors()
                    .into_iter()
                    .find(|monitor| match id {
                        None | Some("display:primary") => monitor.primary,
                        Some(id) => id.strip_prefix("display:") == Some(monitor.id.as_str()),
                    })
                    .ok_or_else(|| "The selected display is no longer available.".to_owned())?;
                let root = self.root_rect().ok_or_else(|| READ_FAILED.to_owned())?;
                let rect = monitor
                    .rect
                    .clip(root.width, root.height)
                    .ok_or_else(|| READ_FAILED.to_owned())?;
                (self.root, rect, (rect.x, rect.y), None)
            };

        let picture = self.read(drawable, rect);
        if let Some(pixmap) = pixmap {
            let _ = self.conn.free_pixmap(pixmap);
        }
        let mut frame = picture?;
        if cursor && self.fixes {
            if let Some(pointer) = xfixes::get_cursor_image(&self.conn)
                .ok()
                .and_then(|reply| reply.reply().ok())
            {
                blend_cursor(&mut frame, origin, &pointer);
            }
        }
        Ok(frame)
    }

    fn window_rect(&self, window: Window) -> Option<(Rect, (i16, i16))> {
        let attributes = self.conn.get_window_attributes(window).ok()?.reply().ok()?;
        if attributes.map_state != MapState::VIEWABLE {
            return None;
        }
        let size = self.conn.get_geometry(window).ok()?.reply().ok()?;
        let at = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        let rect = Rect {
            x: at.dst_x,
            y: at.dst_y,
            width: size.width,
            height: size.height,
        };
        Some((rect, (rect.x, rect.y)))
    }

    fn window_pixmap(&self, window: Window) -> Option<(Pixmap, Rect)> {
        if !self.composite {
            return None;
        }
        let pixmap = self.conn.generate_id().ok()?;
        // A compositor redirects windows. Name that backing pixmap anew each frame so a resize
        // cannot leave the capture pinned to the old backing storage.
        composite::name_window_pixmap(&self.conn, window, pixmap)
            .ok()?
            .check()
            .ok()?;
        if let Some(size) = self
            .conn
            .get_geometry(pixmap)
            .ok()
            .and_then(|reply| reply.reply().ok())
        {
            return Some((
                pixmap,
                Rect {
                    x: 0,
                    y: 0,
                    width: size.width,
                    height: size.height,
                },
            ));
        }
        let _ = self.conn.free_pixmap(pixmap);
        None
    }

    fn read(&mut self, drawable: Drawable, rect: Rect) -> Result<Rgba, String> {
        let expected = usize::from(rect.width) * usize::from(rect.height) * 4;
        let (depth, data) = if let Some(image) = self.read_shared(drawable, rect, expected) {
            image
        } else {
            let reply = self
                .conn
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    drawable,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    !0,
                )
                .ok()
                .and_then(|reply| reply.reply().ok())
                .ok_or_else(|| READ_FAILED.to_owned())?;
            (reply.depth, reply.data)
        };
        let setup = self.conn.setup();
        if !matches!(depth, 24 | 32)
            || data.len() != expected
            || setup.image_byte_order != ImageOrder::LSB_FIRST
        {
            return Err("The selected X11 source has an unsupported pixel format.".to_owned());
        }
        Ok(Rgba {
            width: u32::from(rect.width),
            height: u32::from(rect.height),
            pixels: data,
            bgra: true,
        })
    }

    fn read_shared(
        &mut self,
        drawable: Drawable,
        rect: Rect,
        expected: usize,
    ) -> Option<(u8, Vec<u8>)> {
        if !self.shm {
            return None;
        }
        if self
            .shared
            .as_ref()
            .is_none_or(|image| image.map.len() < expected)
        {
            if let Some(old) = self.shared.take() {
                let _ = shm::detach(&self.conn, old.segment);
            }
            let Some(image) = self.allocate_shared(expected) else {
                self.shm = false;
                return None;
            };
            self.shared = Some(image);
        }
        let image = self.shared.as_ref()?;
        let reply = shm::get_image(
            &self.conn,
            drawable,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            !0,
            u8::from(ImageFormat::Z_PIXMAP),
            image.segment,
            0,
        )
        .ok()?
        .reply()
        .ok()?;
        if reply.size as usize != expected {
            return None;
        }
        // The server completed GetImage and no other request can write the mapping while it is read.
        let pixels = unsafe { std::slice::from_raw_parts(image.map.as_ptr(), expected) }.to_vec();
        Some((reply.depth, pixels))
    }

    fn allocate_shared(&self, size: usize) -> Option<SharedImage> {
        let segment = self.conn.generate_id().ok()?;
        let reply = shm::create_segment(&self.conn, segment, size.try_into().ok()?, false)
            .ok()?
            .reply()
            .ok()?;
        match memmap2::MmapOptions::new()
            .len(size)
            .map_raw_read_only(&reply.shm_fd)
        {
            Ok(map) => Some(SharedImage { segment, map }),
            Err(_) => {
                let _ = shm::detach(&self.conn, segment);
                None
            }
        }
    }
}

fn blend_cursor(frame: &mut Rgba, origin: (i16, i16), pointer: &xfixes::GetCursorImageReply) {
    let left = i32::from(pointer.x) - i32::from(pointer.xhot) - i32::from(origin.0);
    let top = i32::from(pointer.y) - i32::from(pointer.yhot) - i32::from(origin.1);
    for (i, pixel) in pointer.cursor_image.iter().copied().enumerate() {
        let x = left + (i % usize::from(pointer.width)) as i32;
        let y = top + (i / usize::from(pointer.width)) as i32;
        if x < 0 || y < 0 || x >= frame.width as i32 || y >= frame.height as i32 {
            continue;
        }
        // XFixes returns premultiplied ARGB. Blend BGR over the opaque capture.
        let dest = ((y as u32 * frame.width + x as u32) * 4) as usize;
        let inverse = 255 - (pixel >> 24);
        for channel in 0..3 {
            let source = (pixel >> (8 * channel)) & 255;
            let behind = u32::from(frame.pixels[dest + channel]);
            frame.pixels[dest + channel] = (source + (behind * inverse + 127) / 255).min(255) as u8;
        }
    }
}
