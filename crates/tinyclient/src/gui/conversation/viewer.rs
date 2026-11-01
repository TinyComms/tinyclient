//! The image viewer: one picture over the whole window, at fit or at its own size, with the
//! rest of the conversation's pictures a key away.

use super::draw::open_file;
use super::icons::{icon, Icon};
use super::images::Picture;
use super::media::{self, Shape};
use super::tokens;
use super::Pane;
use crate::gui::anim::{self, Play};
use crate::gui::components::{tip, Avatar};
use crate::gui::chrome;
use crate::gui::layout::TOP_BAR;
use crate::gui::theme::{self, Type};
use crate::gui::time;
use crate::view::now;
use gpui::{
    anchored, deferred, div, point, prelude::*, px, AnyElement, ClipboardItem, Context, Div, ElementId, Image,
    ImageFormat, KeyDownEvent, MouseButton, ObjectFit, Pixels, Point, Stateful, Window,
};
use protocol::{Attachment, Message};
use std::path::PathBuf;
use std::rc::Rc;

/// The longest side the original is ever decoded to, however far it is zoomed: 64 MB.
const MOST_EDGE: u32 = 4096;
const FOOTER: f32 = 72.;
const SIDE: f32 = 64.;
/// The stops the zoom buttons and keys move between.
const STOPS: [f32; 11] = [0.1, 0.25, 0.5, 0.75, 1., 1.5, 2., 3., 4., 6., 8.];

/// The zoom that fits a picture of `natural` pixels in `room` points at `scale`, in screen
/// pixels a picture pixel. Never past one point a pixel, as its row draws it: a small picture
/// opens no larger than it was shown.
pub fn fit_zoom(natural: (f32, f32), room: (f32, f32), scale: f32) -> f32 {
    let (wide, tall) = (natural.0.max(1.), natural.1.max(1.));
    (room.0 / wide).min(room.1 / tall).min(1.) * scale
}

/// The next stop up or down from `zoom`. Fit is rarely on a stop, so it moves to the nearest
/// one in that direction.
pub fn step_zoom(zoom: f32, closer: bool) -> f32 {
    let near = |stop: f32| (stop - zoom).abs() < 0.005;
    if closer {
        STOPS.iter().copied().find(|stop| *stop > zoom && !near(*stop)).unwrap_or(STOPS[STOPS.len() - 1])
    } else {
        STOPS.iter().rev().copied().find(|stop| *stop < zoom && !near(*stop)).unwrap_or(STOPS[0])
    }
}

/// The longest side to decode the original to: what it covers on screen at this zoom, in
/// device pixels, and never more than it has or than [`MOST_EDGE`]. Fitted in a small window, a
/// large photo costs the window's pixels, not its own.
pub fn viewer_edge(natural: (u32, u32), zoom: f32) -> u32 {
    let longest = natural.0.max(natural.1);
    if longest == 0 {
        return MOST_EDGE;
    }
    let shown = (longest as f32 * zoom).ceil() as u32;
    shown.clamp(1, longest.min(MOST_EDGE))
}

/// Keeps a pan within what there is to see: a side that fits the room stays centered, and one
/// that does not can be dragged until its edge reaches the room's.
pub fn clamp_pan(pan: (f32, f32), drawn: (f32, f32), room: (f32, f32)) -> (f32, f32) {
    let most = |drawn: f32, room: f32| ((drawn - room) / 2.).max(0.);
    let (x, y) = (most(drawn.0, room.0), most(drawn.1, room.1));
    (pan.0.clamp(-x, x), pan.1.clamp(-y, y))
}

/// What the clipboard is told a file is, from how it starts.
fn clipboard_format(head: &[u8]) -> Option<ImageFormat> {
    match head {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', ..] => Some(ImageFormat::Gif),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        [b'I', b'I', ..] | [b'M', b'M', ..] => Some(ImageFormat::Tiff),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

pub struct Viewer {
    message: String,
    attachment: usize,
    /// Screen pixels a picture pixel, so 100% is the picture's own pixels whatever the screen's
    /// scale. `None` is fitted to the window.
    zoom: Option<f32>,
    /// How far the picture is dragged from the middle, in points.
    pan: (f32, f32),
    /// The pointer and the pan when a drag began.
    grab: Option<(Point<Pixels>, (f32, f32))>,
    /// The drag moved, so the release is not a click.
    dragged: bool,
    /// What the last save or copy said.
    note: Option<String>,
    /// The sizes the original was asked to be decoded at, as `(path, edge)`: each goes once a
    /// larger or smaller one shows, and all of them when the viewer moves on or closes.
    decoded: Vec<(String, u32)>,
}

/// What is kept around the picture: the sides, the bar above it, the caption below. A phone
/// shows the picture edge to edge, under the status bar's band and its own strip, over the home
/// indicator's.
fn margins() -> (f32, f32, f32) {
    if crate::gui::phone::active() {
        use crate::gui::phone::tokens;
        (0., tokens::safe_top() + tokens::NAV, FOOTER + tokens::safe_bottom())
    } else {
        (SIDE, TOP_BAR, FOOTER)
    }
}

/// The space the picture is fitted into, for a window this size.
fn room_in(wide: f32, tall: f32) -> (f32, f32) {
    let (side, top, bottom) = margins();
    ((wide - 2. * side).max(1.), (tall - top - bottom).max(1.))
}

fn bar_button(id: impl Into<ElementId>, which: Icon) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(32.))
        .flex_none()
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(which, 16., theme::text()))
}

fn side_button(id: impl Into<ElementId>, which: Icon) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .size(px(40.))
        .rounded_full()
        .bg(theme::overlay())
        .border_1()
        .border_color(theme::overlay_edge())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(tokens::float_hover()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(which, 20., theme::text()))
}

impl Pane {
    /// Every picture held, oldest first: single pictures, grid tiles and GIFs. Videos are not
    /// pictures, and a spoiler nobody opened stays closed.
    fn pictures(&self) -> Vec<(Rc<Message>, usize)> {
        let root = self.open.as_ref().and_then(|open| open.root.as_ref()).map(|(root, _)| root);
        root.into_iter()
            .chain(&self.window.messages)
            .flat_map(|message| {
                message.attachments.iter().enumerate().filter_map(move |(ix, attachment)| {
                    // A GIF that is an MP4 plays in the video player, not here.
                    let shown = match media::shape(attachment) {
                        Shape::Image => true,
                        Shape::Gif => !media::is_video(attachment),
                        _ => false,
                    };
                    let open = !media::is_spoiler(attachment) || self.shown.contains(&attachment.file.id);
                    (shown && open).then(|| (message.clone(), ix))
                })
            })
            .collect()
    }

    pub(super) fn open_viewer(&mut self, message: &str, attachment: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.let_go_originals();
        self.viewer = Some(Viewer {
            message: message.to_owned(),
            attachment,
            zoom: None,
            pan: (0., 0.),
            grab: None,
            dragged: false,
            note: None,
            decoded: Vec::new(),
        });
        self.hover = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close_viewer(&mut self, cx: &mut Context<Self>) {
        self.let_go_originals();
        self.viewer = None;
        cx.notify();
    }

    /// A full-screen original is the largest thing a pane decodes: it goes as soon as the
    /// viewer stops showing it, not when the budget next needs the room.
    fn let_go_originals(&mut self) {
        if let Some(viewer) = self.viewer.as_mut() {
            self.let_go.append(&mut viewer.decoded);
        }
    }

    fn viewed(&self) -> Option<(Rc<Message>, Attachment)> {
        let viewer = self.viewer.as_ref()?;
        let message = self.find(&viewer.message)?;
        let attachment = message.attachments.get(viewer.attachment)?.clone();
        Some((message, attachment))
    }

    /// The next picture, or the one before.
    pub(super) fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let pictures = self.pictures();
        let Some(viewer) = self.viewer.as_mut() else {
            return;
        };
        let at = pictures.iter().position(|(message, ix)| message.id == viewer.message && *ix == viewer.attachment);
        let Some(next) = at.and_then(|at| at.checked_add_signed(by)).and_then(|at| pictures.get(at)) else {
            return;
        };
        self.let_go.append(&mut viewer.decoded);
        viewer.message.clone_from(&next.0.id);
        viewer.attachment = next.1;
        viewer.zoom = None;
        viewer.pan = (0., 0.);
        viewer.note = None;
        cx.notify();
    }

    /// Fit (`None`) or so many screen pixels a picture pixel.
    pub(super) fn zoom(&mut self, zoom: Option<f32>, cx: &mut Context<Self>) {
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.zoom = zoom.map(|zoom| zoom.clamp(STOPS[0], STOPS[STOPS.len() - 1]));
            if zoom.is_none() {
                viewer.pan = (0., 0.);
            }
            cx.notify();
        }
    }

    /// One stop in or out from where it is. `fit` is what fitted comes to now.
    fn zoom_step(&mut self, closer: bool, fit: f32, cx: &mut Context<Self>) {
        let now = self.viewer.as_ref().and_then(|viewer| viewer.zoom).unwrap_or(fit);
        self.zoom(Some(step_zoom(now, closer)), cx);
    }

    /// What fitted comes to for the picture on show, from its size and the window's.
    fn fit_now(&self, window: &Window) -> f32 {
        let Some((_, attachment)) = self.viewed() else {
            return 1.;
        };
        let size = window.viewport_size();
        let room = room_in(f32::from(size.width), f32::from(size.height));
        fit_zoom((attachment.file.width as f32, attachment.file.height as f32), room, window.scale_factor())
    }

    /// Puts the original on the clipboard as the picture it is, not as a path.
    fn copy_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.viewed().and_then(|(_, attachment)| attachment.file.path) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(path).ok()?;
                    Some(Image::from_bytes(clipboard_format(&bytes)?, bytes))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(image) = &read {
                    cx.write_to_clipboard(ClipboardItem::new_image(image));
                }
                if let Some(viewer) = this.viewer.as_mut() {
                    viewer.note = Some(if read.is_some() { "Copied".to_owned() } else { "Couldn't copy this picture".to_owned() });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Copies the original into Downloads, under a name that is not taken.
    fn save_viewed(&mut self, cx: &mut Context<Self>) {
        let Some((_, attachment)) = self.viewed() else {
            return;
        };
        let Some(from) = attachment.file.path.clone() else {
            return;
        };
        // Only the last part of the name: a file is never written outside Downloads.
        let name = attachment.name.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).unwrap_or("image").to_owned();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    let folder = PathBuf::from(std::env::var_os("HOME")?).join("Downloads");
                    let (stem, extension) = match name.rsplit_once('.') {
                        Some((stem, extension)) => (stem.to_owned(), format!(".{extension}")),
                        None => (name.clone(), String::new()),
                    };
                    let free = (1..100)
                        .map(|n| if n == 1 { folder.join(&name) } else { folder.join(format!("{stem} {n}{extension}")) })
                        .find(|path| !path.exists())?;
                    std::fs::copy(from, &free).ok()?;
                    // Marked as from the internet, so the system checks it when it is opened.
                    if let Err(err) = protocol::quarantine::mark(&free) {
                        tracing::warn!("tinyclient: a saved file was not marked as downloaded: {err}");
                    }
                    Some(free)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(viewer) = this.viewer.as_mut() {
                    viewer.note = Some(match &saved {
                        Some(_) => "Saved to Downloads".to_owned(),
                        None => "Couldn't save to Downloads".to_owned(),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_viewed(&mut self, preview: bool, cx: &mut Context<Self>) {
        let Some((_, attachment)) = self.viewed() else {
            return;
        };
        let name = media::disk_name(&attachment);
        let Some(path) = attachment.file.path else {
            return;
        };
        if preview && crate::gui::links_through_gpui() {
            // A phone has no `xdg-open` or `open -a Preview`: GPUI opens the file's URL here.
            cx.open_url(&format!("file://{path}"));
        } else if preview {
            std::thread::spawn(move || {
                let _ = {
                    #[cfg(target_os = "macos")]
                    {
                        std::process::Command::new("/usr/bin/open").args(["-a", "Preview"]).arg(&path)
                    }
                    #[cfg(target_os = "linux")]
                    {
                        std::process::Command::new("xdg-open").arg(&path)
                    }
                    #[cfg(windows)]
                    {
                        let mut command = std::process::Command::new("cmd");
                        protocol::launch::no_console(command.args(["/C", "start", "", &path]));
                        command
                    }
                    // Other targets (phones) take the GPUI branch above; this keeps them compiling.
                    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
                    {
                        std::process::Command::new("true")
                    }
                }
                .status();
            });
        } else {
            open_file(&path, &name, super::Then::QuickLook, cx);
        }
    }

    /// True when the key was the viewer's.
    pub(super) fn viewer_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keys = &event.keystroke;
        let command = crate::gui::shortcuts::primary(keys.modifiers);
        let fit = self.fit_now(window);
        match (keys.key.as_str(), command) {
            ("escape", _) => self.close_viewer(cx),
            ("left", false) => self.step(-1, cx),
            ("right", false) => self.step(1, cx),
            ("0", true) => self.zoom(None, cx),
            ("1", true) => self.zoom(Some(1.), cx),
            ("=" | "+", _) => self.zoom_step(true, fit, cx),
            ("-", _) => self.zoom_step(false, fit, cx),
            ("c", true) => self.copy_viewed(cx),
            ("s", true) => self.save_viewed(cx),
            ("space", false) | ("y", true) => self.open_viewed(false, cx),
            _ => return false,
        }
        true
    }

    pub(super) fn viewer_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (message, attachment) = self.viewed()?;
        let (asked, pan, note) = self.viewer.as_ref().map(|viewer| (viewer.zoom, viewer.pan, viewer.note.clone()))?;
        let pictures = self.pictures();
        let at = pictures.iter().position(|(held, ix)| held.id == message.id && Some(*ix) == self.viewer.as_ref().map(|v| v.attachment));
        let size = window.viewport_size();
        let (wide, tall) = (f32::from(size.width), f32::from(size.height));
        let room = room_in(wide, tall);
        let scale = window.scale_factor();
        let phone = crate::gui::phone::active();

        // The original, once it is here; the still from the row stands in until then.
        let full = &attachment.file;
        let still = attachment.thumb.as_ref().unwrap_or(full);
        let fit_to = |wide: u32, tall: u32| fit_zoom((wide as f32, tall as f32), room, scale);
        let edge = viewer_edge((full.width, full.height), asked.unwrap_or_else(|| fit_to(full.width, full.height)));
        let original = match &full.path {
            Some(path) => {
                let picture = self.images.get_to(path, edge, None, Play::Asked, cx, Self::decoded);
                if let Some(viewer) = self.viewer.as_mut() {
                    let now = (path.clone(), edge);
                    if !viewer.decoded.contains(&now) {
                        viewer.decoded.push(now.clone());
                    }
                    // Once this size shows, the sizes asked for before it are not needed.
                    if matches!(picture, Picture::Ready(_)) && viewer.decoded.len() > 1 {
                        let before = viewer.decoded.iter().filter(|held| **held != now).cloned();
                        self.let_go.extend(before);
                        viewer.decoded = vec![now];
                    }
                }
                match picture {
                    // Zoomed to a new size: the size before stands in until this one is read.
                    Picture::Loading => self.images.largest(path).map_or(Picture::Loading, Picture::Ready),
                    other => other,
                }
            }
            None if full.failed.is_some() => Picture::Failed,
            None => {
                // The whole file, whatever size its still was asked for at.
                self.want(full, full.width.max(full.height).max(1));
                Picture::Loading
            }
        };
        let (shown, loading, failed) = match original {
            Picture::Ready(image) => (Some(image), false, false),
            other => {
                let edge = media::decode_edge((still.width, still.height), room.0, room.1, media::Fit::Contain, scale);
                let stand_in = still.path.as_deref().filter(|_| still.id != full.id).and_then(|path| {
                    match self.picture(path, edge, None, cx) {
                        Picture::Ready(image) => Some(image),
                        // The row's decode, whatever its size, until this one is read.
                        _ => self.images.largest(path),
                    }
                });
                (stand_in, matches!(other, Picture::Loading), matches!(other, Picture::Failed))
            }
        };
        // Its size as the network gave it, or as decoded when it did not say.
        let natural = match (full.width, full.height, &shown) {
            (w, h, _) if w > 0 && h > 0 => (w as f32, h as f32),
            (_, _, Some(image)) => (image.size(0).width.0 as f32, image.size(0).height.0 as f32),
            _ => (room.0, room.1),
        };
        let fit = fit_zoom(natural, room, scale);
        let zoom = asked.unwrap_or(fit);
        // In points: `zoom` is screen pixels a picture pixel.
        let drawn = ((natural.0 * zoom / scale).round(), (natural.1 * zoom / scale).round());
        let pan = clamp_pan(pan, drawn, room);
        let pannable = drawn.0 > room.0 || drawn.1 > room.1;

        let sender = if message.outgoing { "You".to_owned() } else { message.sender.name.clone() };
        let place = self.open.as_ref().map(|open| open.title.clone()).unwrap_or_default();
        let mut facts = vec![place, attachment.name.clone()];
        if full.width > 0 && full.height > 0 {
            facts.push(format!("{} × {}", full.width, full.height));
        }
        facts.retain(|fact| !fact.is_empty());

        let bar = if phone {
            self.phone_bar(&message, &sender, &facts, cx)
        } else {
            self.desktop_bar(&message, &sender, &facts, asked, zoom, fit, window, cx)
        };

        let picture = match shown {
            Some(image) => div()
                .id("picture")
                .absolute()
                .left(px(((wide - drawn.0) / 2. + pan.0).round()))
                .top(px(((room.1 - drawn.1) / 2. + pan.1).round()))
                .w(px(drawn.0))
                .h(px(drawn.1))
                .cursor(if pannable { gpui::CursorStyle::OpenHand } else { gpui::CursorStyle::PointingHand })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(viewer) = this.viewer.as_mut() {
                            viewer.grab = Some((event.position, pan));
                            viewer.dragged = false;
                        }
                    }),
                )
                // A click that was not a drag goes between fitted and the picture's own size.
                .on_click(cx.listener(move |this, _, _, cx| {
                    let dragged = this.viewer.as_ref().is_some_and(|viewer| viewer.dragged);
                    if !dragged {
                        this.zoom(if asked.is_some() { None } else { Some(1.) }, cx);
                    }
                }))
                // Opened on purpose: a moving picture plays here whatever the autoplay setting.
                .child(anim::picture(image, ObjectFit::Fill, 0., Play::Asked))
                .into_any_element(),
            None => div()
                .type_ui()
                .text_color(theme::text_soft())
                .child(if failed { "Couldn't load the full image" } else { "Loading…" })
                .into_any_element(),
        };
        let stage = div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            // The original is on its way: the bar moves only when a file lands, never on a timer.
            .children(loading.then(|| div().absolute().top_0().left_0().w(px(wide * 0.3)).h(px(2.)).bg(theme::accent())))
            .child(picture)
            .on_scroll_wheel(cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(20.));
                if crate::gui::shortcuts::primary(event.modifiers) {
                    // A notch at a time, however far the wheel says it turned.
                    if delta.y != px(0.) {
                        this.zoom_step(delta.y > px(0.), fit, cx);
                    }
                } else if let Some(viewer) = this.viewer.as_mut().filter(|_| pannable) {
                    viewer.pan = clamp_pan((pan.0 + f32::from(delta.x), pan.1 + f32::from(delta.y)), drawn, room);
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(at.filter(|at| *at > 0).map(|_| {
                side_button("previous", Icon::ChevronLeft)
                    .left(px(12.))
                    .top(px((room.1 - 40.) / 2.))
                    .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx)))
            }))
            .children(at.filter(|at| at + 1 < pictures.len()).map(|_| {
                side_button("next", Icon::ChevronRight)
                    .right(px(12.))
                    .top(px((room.1 - 40.) / 2.))
                    .on_click(cx.listener(|this, _, _, cx| this.step(1, cx)))
            }));
        // A phone's narrow caption says "1 of 1" for no one: only a run of pictures is counted.
        let count = at.filter(|_| !phone || pictures.len() > 1).map(|at| format!("{} of {}", at + 1, pictures.len()));
        // Who sent it and when, then what they said with it.
        let caption = [count, Some(sender), Some(format!("{} {}", time::day(now(), message.sent), time::clock(message.sent)))];
        let caption = caption.into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let said = message.text.lines().next().filter(|line| !line.trim().is_empty()).map(str::to_owned);
        let footer = div()
            .h(px(FOOTER))
            .flex_none()
            // A phone's caption may take more lines than the desktop's one: it grows, over the
            // home indicator's band.
            .when(phone, |footer| footer.h_auto().min_h(px(margins().2)).pt(px(8.)).pb(px(8. + crate::gui::phone::tokens::safe_bottom())).px(px(16.)).text_center())
            .px(px(SIDE))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .type_small()
            .text_color(theme::text_soft())
            .child(div().max_w_full().child(caption))
            // A phone has the width for two lines of what was said, and no wider window to read it in.
            .children(said.map(|said| {
                div().max_w_full().when(!phone, |line| line.truncate()).when(phone, |line| line.line_clamp(2).text_center()).text_color(theme::text()).child(said)
            }))
            .children(note.map(|note| div().text_color(theme::accent_text()).child(note)));

        let layer = div()
            .id("viewer")
            .w(size.width)
            .h(size.height)
            .flex()
            .flex_col()
            // A phone's viewer is the whole screen, black, as its own photos are.
            .bg(if phone { gpui::rgb(0x000000) } else { tokens::scrim() })
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            // A drag that began on the picture goes on wherever the pointer is.
            .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                let Some(viewer) = this.viewer.as_mut() else {
                    return;
                };
                let Some((from, start)) = viewer.grab.filter(|_| event.pressed_button == Some(MouseButton::Left)) else {
                    viewer.grab = None;
                    return;
                };
                let moved = (f32::from(event.position.x - from.x), f32::from(event.position.y - from.y));
                if moved.0.abs() + moved.1.abs() > 3. {
                    viewer.dragged = true;
                }
                if pannable && viewer.dragged {
                    viewer.pan = clamp_pan((start.0 + moved.0, start.1 + moved.1), drawn, room);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                // The release of a drag that ran off the picture is not a click on the scrim.
                if this.viewer.as_mut().is_some_and(|viewer| std::mem::take(&mut viewer.dragged)) {
                    return;
                }
                this.close_viewer(cx);
            }))
            .child(bar)
            .child(stage)
            .child(footer);
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(layer)).priority(2).into_any_element())
    }

    /// The desktop's bar: who sent it, then zoom, copy, save, Quick Look, Preview and close.
    #[allow(clippy::too_many_arguments)]
    fn desktop_bar(&self, message: &Rc<Message>, sender: &str, facts: &[String], asked: Option<f32>, zoom: f32, fit: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let sender = sender.to_owned();
        div()
            .h(px(TOP_BAR))
            .flex_none()
            // Clear of the window's own buttons.
            .pl(px(84.))
            .pr(px(chrome::corner_pad(window, 12.)))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(Avatar::named(&message.sender.name, 24.).image(message.sender.avatar.as_ref()).build())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().type_ui().font_weight(theme::SEMIBOLD).child(sender.clone()))
                    .child(div().truncate().type_micro().font_weight(theme::REGULAR).text_color(theme::text_soft()).child(facts.join(" · "))),
            )
            .child(
                bar_button("zoom-out", Icon::ZoomOut)
                    .tooltip(tip("Zoom out", Some("−")))
                    .on_click(cx.listener(move |this, _, _, cx| this.zoom_step(false, fit, cx))),
            )
            .child(
                div()
                    .id("zoom")
                    .h(px(32.))
                    .w(px(56.))
                    .flex_none()
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .type_ui()
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .tooltip(tip(if asked.is_some() { "Fit to window" } else { "Actual size" }, Some(if asked.is_some() { "⌘0" } else { "⌘1" })))
                    .on_click(cx.listener(move |this, _, _, cx| this.zoom(if asked.is_some() { None } else { Some(1.) }, cx)))
                    .child(if asked.is_none() { "Fit".to_owned() } else { format!("{:.0}%", zoom * 100.) }),
            )
            .child(
                bar_button("zoom-in", Icon::ZoomIn)
                    .tooltip(tip("Zoom in", Some("+")))
                    .on_click(cx.listener(move |this, _, _, cx| this.zoom_step(true, fit, cx))),
            )
            .child(
                bar_button("fit", Icon::Fit)
                    .tooltip(tip("Fit to window", Some("⌘0")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(None, cx))),
            )
            .child(
                div()
                    .id("actual")
                    .h(px(32.))
                    .px(px(8.))
                    .flex_none()
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .type_small()
                    .font_weight(theme::MEDIUM)
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .tooltip(tip("Actual size", Some("⌘1")))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom(Some(1.), cx)))
                    .child("1:1"),
            )
            .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).flex_none().bg(theme::hairline()))
            .child(
                bar_button("copy", Icon::Copy)
                    .tooltip(tip("Copy image", Some("⌘C")))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_viewed(cx))),
            )
            .child(
                bar_button("save", Icon::Download)
                    .tooltip(tip("Save to Downloads", Some("⌘S")))
                    .on_click(cx.listener(|this, _, _, cx| this.save_viewed(cx))),
            )
            .child(
                bar_button("look", Icon::Eye)
                    .tooltip(tip("Quick Look", Some("Space")))
                    .on_click(cx.listener(|this, _, _, cx| this.open_viewed(false, cx))),
            )
            .child(
                bar_button("preview", Icon::External)
                    .tooltip(tip("Open in Preview", None))
                    .on_click(cx.listener(|this, _, _, cx| this.open_viewed(true, cx))),
            )
            .child(
                bar_button("close", Icon::Close)
                    .tooltip(tip("Close", Some("esc")))
                    .on_click(cx.listener(|this, _, _, cx| this.close_viewer(cx))),
            )
    }

    /// A phone's bar, under the status bar: close on the left as every pushed page has back,
    /// who sent it, then copy and save. A tap on the picture goes between fitted and its own
    /// size; a phone has no keys to zoom by and no Preview to open it in.
    fn phone_bar(&self, message: &Rc<Message>, sender: &str, facts: &[String], cx: &mut Context<Self>) -> Div {
        use crate::gui::phone::tokens::{self as phone, PhoneType};
        let action = |id: &'static str, which: Icon| bar_button(id, which).size(px(phone::TOUCH));
        div()
            .flex_none()
            .pt(px(phone::safe_top()))
            .child(
                div()
                    .h(px(phone::NAV))
                    .pl(px(2.))
                    .pr(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(action("close", Icon::Close).on_click(cx.listener(|this, _, _, cx| this.close_viewer(cx))))
                    .child(Avatar::named(&message.sender.name, 28.).image(message.sender.avatar.as_ref()).build())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().phone_row().font_weight(theme::SEMIBOLD).child(sender.to_owned()))
                            .child(div().truncate().phone_stamp().font_weight(theme::REGULAR).text_color(theme::text_soft()).child(facts.join(" · "))),
                    )
                    .child(action("copy", Icon::Copy).on_click(cx.listener(|this, _, _, cx| this.copy_viewed(cx))))
                    .child(action("save", Icon::Download).on_click(cx.listener(|this, _, _, cx| this.save_viewed(cx)))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_moves_a_stop_at_a_time_and_stays_between_a_tenth_and_eight() {
        // From a fit of 37%: in to 50, out to 25.
        assert_eq!(step_zoom(0.37, true), 0.5);
        assert_eq!(step_zoom(0.37, false), 0.25);
        // On a stop, the next one; not the same one again.
        assert_eq!(step_zoom(1., true), 1.5);
        assert_eq!(step_zoom(1., false), 0.75);
        assert_eq!(step_zoom(8., true), 8.);
        assert_eq!(step_zoom(0.1, false), 0.1);
    }

    #[test]
    fn the_original_is_decoded_to_what_the_window_shows_of_it() {
        // 2400 x 1800 fitted at 41% on a 2x screen: 1968 px across, not 2400.
        assert_eq!(viewer_edge((2400, 1800), 0.82), 1968);
        // At its own size and beyond, all it has; a huge one stops at the limit.
        assert_eq!(viewer_edge((2400, 1800), 1.), 2400);
        assert_eq!(viewer_edge((2400, 1800), 4.), 2400);
        assert_eq!(viewer_edge((12000, 9000), 1.), MOST_EDGE);
        assert_eq!(viewer_edge((0, 0), 1.), MOST_EDGE);
    }

    #[test]
    fn fitted_is_in_screen_pixels_and_never_past_a_point_a_pixel() {
        // A 2400 x 1800 photo in a 1000 x 600 point room: a third of a point a pixel, which on
        // a 2x screen is two thirds of a screen pixel each, 1600 across.
        let fit = fit_zoom((2400., 1800.), (1000., 600.), 2.);
        assert!((fit - 2. / 3.).abs() < 1e-6, "{fit}");
        assert_eq!(viewer_edge((2400, 1800), fit), 1600);
        // The same at 1x: half as many screen pixels, so half the decode.
        assert_eq!(viewer_edge((2400, 1800), fit_zoom((2400., 1800.), (1000., 600.), 1.)), 800);
        // A small picture is drawn at a point a pixel, as in its row: 200% on a 2x screen.
        assert_eq!(fit_zoom((640., 480.), (1000., 600.), 2.), 2.);
        assert_eq!(fit_zoom((640., 480.), (1000., 600.), 1.), 1.);
    }

    #[test]
    fn a_picture_can_be_dragged_to_its_edges_and_no_further() {
        // 2000 x 600 in a room of 1000 x 800: sideways by 500 either way, never up or down.
        assert_eq!(clamp_pan((900., 40.), (2000., 600.), (1000., 800.)), (500., 0.));
        assert_eq!(clamp_pan((-900., -40.), (2000., 600.), (1000., 800.)), (-500., 0.));
        assert_eq!(clamp_pan((120., 0.), (2000., 600.), (1000., 800.)), (120., 0.));
        // Fitted: nowhere to go.
        assert_eq!(clamp_pan((50., 50.), (500., 400.), (1000., 800.)), (0., 0.));
    }

    #[test]
    fn a_copied_picture_is_put_on_the_clipboard_as_what_it_is() {
        assert_eq!(clipboard_format(b"\x89PNG\r\n\x1a\n...."), Some(ImageFormat::Png));
        assert_eq!(clipboard_format(b"\xff\xd8\xff\xe0"), Some(ImageFormat::Jpeg));
        assert_eq!(clipboard_format(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageFormat::Webp));
        assert_eq!(clipboard_format(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(clipboard_format(b"plain text"), None);
    }
}
