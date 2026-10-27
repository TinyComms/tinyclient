//! The window's own buttons and edges, where the platform leaves them to the window.
//!
//! The 52 pt strips stand in for a title bar. macOS draws the traffic lights over them. Windows
//! draws nothing once its caption is hidden, so the window draws minimize, maximize and close
//! at the right end of the top strip and tells Windows where they are: Windows then presses
//! them, snaps from maximize and drags from the strips itself. Linux asks the compositor for its
//! own title bar (KDE, Sway, X11 window managers draw one); one that leaves decorations to the
//! window, GNOME on Wayland, gets the same three buttons and edges that resize.

use super::components::glyph;
use super::layout::TOP_BAR;
use super::theme;
use gpui::{
    deferred, div, prelude::*, px, rgb, AnyElement, App, ClickEvent, CursorStyle, Decorations, Div, MouseButton, ResizeEdge, Rgba,
    Stateful, Window, WindowControlArea,
};
use std::cell::Cell;

/// Windows' caption buttons: 46 wide, 32 tall, flush with the corner.
const CAPTION: (f32, f32) = (46., 32.);
/// Windows' close button under the pointer.
const CLOSE_RED: u32 = 0xc42b1c;
/// GNOME's round buttons, centered in the strip.
const ROUND: f32 = 24.;
const ROUND_GAP: f32 = 8.;
const ROUND_MARGIN: f32 = 12.;
/// Above the image viewer's layer.
const PRIORITY: usize = 3;
/// Between the buttons and whatever the strip draws to their left.
const CLEAR: f32 = 8.;
/// How far in from a Linux window's edge a press resizes it.
const EDGE: f32 = 5.;

/// Who draws the buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    /// The platform: the traffic lights, or a compositor's title bar.
    Platform,
    Windows,
    Linux,
}

fn style(window: &Window) -> Style {
    if cfg!(target_os = "macos") || window.is_fullscreen() {
        Style::Platform
    } else if cfg!(windows) {
        Style::Windows
    } else {
        match window.window_decorations() {
            Decorations::Client { .. } => Style::Linux,
            Decorations::Server => Style::Platform,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Button {
    Minimize,
    Maximize,
    Close,
}

fn buttons_of(window: &Window) -> Vec<Button> {
    let offered = window.window_controls();
    let mut buttons = Vec::with_capacity(3);
    if window.is_minimizable() && offered.minimize {
        buttons.push(Button::Minimize);
    }
    if window.is_resizable() && offered.maximize {
        buttons.push(Button::Maximize);
    }
    buttons.push(Button::Close);
    buttons
}

/// How much of the top strip's right end the buttons cover, 0 where the platform draws them.
pub fn width(window: &Window) -> f32 {
    let count = buttons_of(window).len() as f32;
    match style(window) {
        Style::Platform => 0.,
        Style::Windows => count * CAPTION.0,
        Style::Linux => count * ROUND + (count - 1.) * ROUND_GAP + ROUND_MARGIN,
    }
}

/// The region whose strip ends the row at the window's right edge, under the buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// The conversation column: a conversation, Tiny's page, or nothing open.
    Conversation,
    /// The right pane.
    Right,
    /// The sidebar, when a narrow window shows the list alone.
    List,
}

thread_local! {
    /// Set by the shell each frame before it draws the regions; the conversation pane, drawn
    /// after it in the same frame, reads it too.
    static END: Cell<(End, f32)> = const { Cell::new((End::Conversation, 0.)) };
}

pub fn set_end(end: End, window: &Window) {
    END.with(|cell| cell.set((end, width(window))));
}

/// The right padding for `at`'s strip: `pad`, or enough to clear the buttons when they sit
/// over it.
pub fn end_pad(at: End, pad: f32) -> f32 {
    match END.with(Cell::get) {
        (end, width) if end == at => clear(width, pad),
        _ => pad,
    }
}

/// The right padding for a strip across the whole window, as the image viewer's.
pub fn corner_pad(window: &Window, pad: f32) -> f32 {
    clear(width(window), pad)
}

fn clear(width: f32, pad: f32) -> f32 {
    if width > 0. { width + CLEAR } else { pad }
}

/// The buttons, over the top-right corner, painted after everything else: the image viewer
/// covers the window and they stay on it. `close` is what the platform's own close button
/// would do to this window.
pub fn buttons(window: &Window, close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Option<AnyElement> {
    let style = style(window);
    if style == Style::Platform {
        return None;
    }
    let maximized = window.is_maximized();
    let row = div().absolute().top_0().right_0().flex().occlude();
    let row = match style {
        Style::Linux => row.h(px(TOP_BAR)).pr(px(ROUND_MARGIN)).items_center().gap(px(ROUND_GAP)),
        _ => row.h(px(CAPTION.1)),
    };
    let mut close = Some(close);
    let row = row.children(buttons_of(window).into_iter().map(|button| {
        let icon = match button {
            Button::Minimize => "icons/minus.svg",
            Button::Maximize if maximized => "icons/copy.svg",
            Button::Maximize => "icons/square.svg",
            Button::Close => "icons/x.svg",
        };
        let id = match button {
            Button::Minimize => "window-minimize",
            Button::Maximize => "window-maximize",
            Button::Close => "window-close",
        };
        let face = div().id(id).flex().items_center().justify_center().occlude();
        match style {
            Style::Windows => windows_button(face, button, icon),
            _ => linux_button(face, button, icon)
                .when(button == Button::Close, |face| face.when_some(close.take(), |face, close| face.on_click(close))),
        }
    }));
    Some(deferred(row).with_priority(PRIORITY).into_any_element())
}

/// Windows presses it: a press on a `WindowControlArea` is the caption's own, so the window
/// gets Snap layouts on maximize and closes through `on_window_should_close`.
fn windows_button(face: Stateful<Div>, button: Button, icon: &'static str) -> Stateful<Div> {
    let area = match button {
        Button::Minimize => WindowControlArea::Min,
        Button::Maximize => WindowControlArea::Max,
        Button::Close => WindowControlArea::Close,
    };
    let (hover, pressed): (Rgba, Rgba) = match button {
        Button::Close => (rgb(CLOSE_RED), Rgba { a: 0.8, ..rgb(CLOSE_RED) }),
        _ => (theme::hover(), theme::pressed()),
    };
    face.w(px(CAPTION.0))
        .h_full()
        .window_control_area(area)
        .hover(|style| style.bg(hover))
        .active(|style| style.bg(pressed))
        .child(glyph(icon, 14., theme::text_soft()))
}

/// GNOME's round buttons. The compositor knows nothing of them, so a click does the work;
/// the caller gives close its own.
fn linux_button(face: Stateful<Div>, button: Button, icon: &'static str) -> Stateful<Div> {
    face.size(px(ROUND))
        .rounded_full()
        .bg(theme::hover())
        .hover(|style| style.bg(theme::pressed()))
        .active(|style| style.bg(theme::pressed()))
        .cursor(CursorStyle::Arrow)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(button != Button::Close, |face| {
            face.on_click(move |_, window, _| match button {
                Button::Minimize => window.minimize_window(),
                _ => window.zoom_window(),
            })
        })
        .child(glyph(icon, 14., theme::text()))
}

/// Where a Linux window that draws its own decorations resizes: a band along each edge it
/// does not share with the screen or a tiled neighbour. Elsewhere the platform resizes.
pub fn edges(window: &Window) -> Option<Div> {
    if style(window) != Style::Linux || !window.is_resizable() || window.is_maximized() {
        return None;
    }
    let Decorations::Client { tiling } = window.window_decorations() else {
        return None;
    };
    let band = |edge: ResizeEdge| {
        let cursor = match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        };
        let band = div()
            .absolute()
            .occlude()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            });
        let (top, bottom, left, right) = (tiling.top, tiling.bottom, tiling.left, tiling.right);
        let band = match edge {
            ResizeEdge::Top => (!top).then(|| band.top_0().left_0().w_full().h(px(EDGE))),
            ResizeEdge::Bottom => (!bottom).then(|| band.bottom_0().left_0().w_full().h(px(EDGE))),
            ResizeEdge::Left => (!left).then(|| band.top_0().left_0().h_full().w(px(EDGE))),
            ResizeEdge::Right => (!right).then(|| band.top_0().right_0().h_full().w(px(EDGE))),
            ResizeEdge::TopLeft => (!top && !left).then(|| band.top_0().left_0().size(px(2. * EDGE))),
            ResizeEdge::TopRight => (!top && !right).then(|| band.top_0().right_0().size(px(2. * EDGE))),
            ResizeEdge::BottomLeft => (!bottom && !left).then(|| band.bottom_0().left_0().size(px(2. * EDGE))),
            ResizeEdge::BottomRight => (!bottom && !right).then(|| band.bottom_0().right_0().size(px(2. * EDGE))),
        };
        band
    };
    // The corners last, so they take the press where two bands meet.
    let all = [
        ResizeEdge::Top,
        ResizeEdge::Bottom,
        ResizeEdge::Left,
        ResizeEdge::Right,
        ResizeEdge::TopLeft,
        ResizeEdge::TopRight,
        ResizeEdge::BottomLeft,
        ResizeEdge::BottomRight,
    ];
    Some(div().absolute().size_full().top_0().left_0().children(all.into_iter().filter_map(band)))
}
