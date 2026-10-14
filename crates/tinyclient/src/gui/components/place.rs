//! Where an overlay goes, so that it stays inside the window (`polish/01-system.md` §4). Every
//! menu, popover, picker, card, switcher, modal and toast is placed through here.
//!
//! How to use it, from any view:
//!
//! ```ignore
//! // A context menu at the pointer:
//! placed("row-menu", Place::Pointer(event.position), Layer::Menu, window, menu().child(..))
//! // A button's menu, 6 below the button, leading edges aligned, flipping above:
//! placed("net-menu", Place::Below(button_bounds), Layer::Menu, window, menu().child(..))
//! // A picker above the composer, right-aligned to its trigger, else below, else shrunk:
//! placed("emoji", Place::Above(trigger_bounds), Layer::Popover, window, picker)
//! // Beside an anchor (a rail button, a submenu's item), flipping to the other side:
//! placed("own", Place::Beside(avatar_bounds), Layer::Popover, window, popover().child(..))
//! ```
//!
//! The element is measured first and then placed: preferred side, then flipped, then shifted
//! along the edge, always 8 from every window edge. A height that cannot fit shrinks (to
//! `H − 16`, or for a picker to the room on its side when that is at least 240) and the content
//! scrolls; a width never shrinks. Anchors are in window coordinates; a view that does not know
//! its anchor's bounds can capture them with `on_children_prepainted` or pass the pointer.
//!
//! The result is `deferred`, painted above the panes in the stacking order of [`Layer`]. One
//! menu or popover is open at a time (plus its submenu); the caller owns closing it.
//!
//! [`fit`] is the arithmetic alone; [`switcher_frame`], [`toast_width`] and [`modal_top`] are
//! the fixed frames.

use crate::gui::theme;
use gpui::{
    deferred, div, point, prelude::*, px, size, AnyElement, App, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, LayoutId, Pixels, Point, Window,
};

/// Every overlay keeps this far from each window edge.
pub const MARGIN: f32 = theme::EDGE;
/// Between an anchor and what it opens.
pub const GAP: f32 = 6.;
/// A picker shrinks to this before it gives up its side.
pub const PICKER_MIN: f32 = 240.;

/// Where an overlay wants to be, relative to what opened it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// A context menu: top-left at the pointer + 2; flips left past the right edge and up past
    /// the bottom.
    Pointer(Point<Pixels>),
    /// A button's menu: 6 below, leading edges aligned; flips above.
    Below(Bounds<Pixels>),
    /// A picker over the composer: 6 above, right-aligned to the trigger; else below; else it
    /// shrinks to at least 240 on the roomier side and scrolls.
    Above(Bounds<Pixels>),
    /// A submenu or a rail popover: beside the anchor, tops aligned; flips to the other side.
    Beside(Bounds<Pixels>),
    /// Centered across the window, its top at `top`: the switcher at 72.
    Top(f32),
    /// Centered, 40 above the window's center: a modal.
    Center,
}

/// Painting order, bottom to top. Tooltips are GPUI's own and sit above all of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// In-pane floats: the jump pill, "2 new ↑".
    Float = 1,
    /// The sidebar floating over a narrow window, with its scrim.
    Sidebar = 2,
    Toast = 3,
    Switcher = 4,
    /// Popovers, pickers, the profile card.
    Popover = 5,
    Menu = 6,
    /// The modal's scrim and the modal.
    Modal = 7,
}

/// A rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

fn rect(bounds: Bounds<Pixels>) -> Frame {
    Frame {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        w: f32::from(bounds.size.width),
        h: f32::from(bounds.size.height),
    }
}

/// Moves `at` so that `[at, at + len]` lies within `[MARGIN, limit − MARGIN]`. Something longer
/// than the room starts at the margin.
fn shift(at: f32, len: f32, limit: f32) -> f32 {
    at.min(limit - MARGIN - len).max(MARGIN)
}

/// The tallest an overlay may be for this place in a window `window` big, before it is measured.
pub fn max_height(place: Place, window: (f32, f32)) -> f32 {
    let (_, tall) = window;
    let full = (tall - 2. * MARGIN).max(0.);
    match place {
        Place::Above(anchor) => {
            let a = rect(anchor);
            let above = a.y - GAP - MARGIN;
            let below = tall - (a.y + a.h) - GAP - MARGIN;
            let roomier = above.max(below);
            if roomier >= PICKER_MIN { roomier } else { full }
        }
        Place::Top(top) => (tall - top - MARGIN).max(0.),
        _ => full,
    }
}

/// Where an overlay of `size` (already capped at [`max_height`]) goes in a window `window` big.
pub fn fit(place: Place, size: (f32, f32), window: (f32, f32)) -> Frame {
    let (w, h) = (size.0, size.1.min(max_height(place, window)));
    let (wide, tall) = window;
    let fits_x = |x: f32| x >= MARGIN && x + w <= wide - MARGIN;
    let fits_y = |y: f32| y >= MARGIN && y + h <= tall - MARGIN;
    let first_fit = |options: &[f32], len: f32, limit: f32, fits: &dyn Fn(f32) -> bool| {
        options.iter().copied().find(|at| fits(*at)).unwrap_or_else(|| shift(options[0], len, limit))
    };
    let (x, y) = match place {
        Place::Pointer(at) => {
            let (px_, py_) = (f32::from(at.x), f32::from(at.y));
            let x = first_fit(&[px_ + 2., px_ - 2. - w], w, wide, &fits_x);
            let y = first_fit(&[py_ + 2., py_ - 2. - h], h, tall, &fits_y);
            (x, y)
        }
        Place::Below(anchor) => {
            let a = rect(anchor);
            let x = first_fit(&[a.x, a.x + a.w - w], w, wide, &fits_x);
            let y = first_fit(&[a.y + a.h + GAP, a.y - GAP - h], h, tall, &fits_y);
            (x, y)
        }
        Place::Above(anchor) => {
            let a = rect(anchor);
            let x = first_fit(&[a.x + a.w - w, a.x], w, wide, &fits_x);
            let above = a.y - GAP - h;
            let below = a.y + a.h + GAP;
            let y = if fits_y(above) {
                above
            } else if fits_y(below) {
                below
            } else {
                // Shrunk to the roomier side: it starts at that side's edge.
                let room_above = a.y - GAP - MARGIN;
                let room_below = tall - (a.y + a.h) - GAP - MARGIN;
                if room_above.max(room_below) >= PICKER_MIN {
                    if room_above >= room_below { a.y - GAP - h } else { below }
                } else {
                    shift(above, h, tall)
                }
            };
            (x, y)
        }
        Place::Beside(anchor) => {
            let a = rect(anchor);
            let x = first_fit(&[a.x + a.w + GAP, a.x - GAP - w], w, wide, &fits_x);
            let y = first_fit(&[a.y, a.y + a.h - h], h, tall, &fits_y);
            (x, y)
        }
        Place::Top(top) => (shift((wide - w) / 2., w, wide), shift(top, h, tall)),
        Place::Center => (shift((wide - w) / 2., w, wide), modal_top(h, tall)),
    };
    Frame { x, y, w, h }
}

/// A modal's top: centered, 40 above center, never above the margin.
pub fn modal_top(height: f32, window_height: f32) -> f32 {
    ((window_height - height) / 2. - 40.).max(MARGIN)
}

/// The switcher: `min(560, W − 48)` wide, centered, top 72, at most `min(20 rows, H − 144)`
/// tall. Answers its frame with `h` as that cap.
pub fn switcher_frame(window: (f32, f32), row: f32) -> Frame {
    let (wide, tall) = window;
    let w = (wide - 48.).min(560.);
    Frame { x: (wide - w) / 2., y: 72., w, h: (20. * row).min(tall - 144.) }
}

/// A toast is `min(340, W − 32)` wide, 16 from the bottom-right.
pub fn toast_width(window_width: f32) -> f32 {
    (window_width - 32.).min(340.)
}

/// Places `content` per `place`, inside the window, painted on `layer`. `content` scrolls when
/// it is taller than the room; the id names that scroll.
pub fn placed(id: impl Into<ElementId>, place: Place, layer: Layer, window: &Window, content: impl IntoElement) -> AnyElement {
    let viewport = window.viewport_size();
    let limit = max_height(place, (f32::from(viewport.width), f32::from(viewport.height)));
    let body = div().id(id).flex_none().max_h(px(limit)).overflow_y_scroll().rounded(px(theme::R_FLOAT))
        // The scroll clips what it holds, so the floating shadow is drawn by the wrapper.
        .shadow(theme::shadow_popover())
        .child(content);
    deferred(Placed { place, child: Some(body.into_any_element()) }).with_priority(layer as usize).into_any_element()
}

/// Measures its one child and puts it where [`fit`] says.
struct Placed {
    place: Place,
    child: Option<AnyElement>,
}

impl IntoElement for Placed {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Placed {
    type RequestLayoutState = Option<LayoutId>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let child = self.child.as_mut().map(|child| child.request_layout(window, cx));
        let style = gpui::Style { position: gpui::Position::Absolute, display: gpui::Display::Flex, ..Default::default() };
        (window.request_layout(style, child, cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (Some(layout), Some(child)) = (*child_layout, self.child.as_mut()) else {
            return;
        };
        let measured = window.layout_bounds(layout).size;
        let viewport = window.viewport_size();
        let frame = fit(
            self.place,
            (f32::from(measured.width), f32::from(measured.height)),
            (f32::from(viewport.width), f32::from(viewport.height)),
        );
        let offset = point(px(frame.x), px(frame.y)) - bounds.origin;
        window.with_element_offset(point(offset.x.round(), offset.y.round()), |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
    }
}

/// Bounds from points, for anchors a view knows by arithmetic.
pub fn anchor(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: (f32, f32) = (1120., 720.);

    #[test]
    fn a_context_menu_opens_at_the_pointer_and_flips_at_the_right_and_bottom_edges() {
        // Room: top-left at the pointer + 2.
        let menu = (220., 200.);
        assert_eq!(fit(Place::Pointer(point(px(300.), px(100.))), menu, WINDOW), Frame { x: 302., y: 102., w: 220., h: 200. });
        // Clicked 40 from the right edge: it opens to the pointer's left.
        let near_right = fit(Place::Pointer(point(px(1080.), px(100.))), menu, WINDOW);
        assert_eq!((near_right.x, near_right.y), (1080. - 2. - 220., 102.));
        // And near the bottom: it opens upward.
        let corner = fit(Place::Pointer(point(px(1080.), px(700.))), menu, WINDOW);
        assert_eq!((corner.x, corner.y), (858., 700. - 2. - 200.));
        assert!(corner.x + corner.w <= WINDOW.0 - MARGIN && corner.y >= MARGIN);
    }

    #[test]
    fn a_menu_that_fits_neither_side_shifts_and_never_leaves_the_window() {
        // A 360 popover in a 540 window, opened at x 200: neither side has room, so it shifts to
        // end 8 from the edge. At x 400 it fits flipped to the pointer's left.
        let popover = fit(Place::Pointer(point(px(200.), px(50.))), (360., 100.), (540., 440.));
        assert_eq!(popover.x, 540. - 8. - 360.);
        let flipped = fit(Place::Pointer(point(px(400.), px(50.))), (360., 100.), (540., 440.));
        assert_eq!(flipped.x, 400. - 2. - 360.);
        // Its width is kept even when the window is narrower than it: it starts at the margin.
        let wide = fit(Place::Below(anchor(10., 10., 20., 20.)), (600., 100.), (540., 440.));
        assert_eq!((wide.x, wide.w), (MARGIN, 600.));
        // A menu taller than the window shrinks to H − 16 and starts at the margin.
        let tall = fit(Place::Pointer(point(px(100.), px(300.))), (220., 900.), (540., 440.));
        assert_eq!((tall.y, tall.h), (MARGIN, 424.));
    }

    #[test]
    fn a_button_menu_drops_below_aligned_and_flips_above_near_the_bottom() {
        let button = anchor(500., 40., 28., 28.);
        assert_eq!(fit(Place::Below(button), (220., 150.), WINDOW), Frame { x: 500., y: 74., w: 220., h: 150. });
        // At the right edge its trailing edge aligns with the button's instead.
        let right = fit(Place::Below(anchor(1060., 40., 28., 28.)), (220., 150.), WINDOW);
        assert_eq!(right.x, 1088. - 220.);
        // Near the bottom it opens above the button.
        let low = fit(Place::Below(anchor(500., 650., 28., 28.)), (220., 150.), WINDOW);
        assert_eq!(low.y, 650. - 6. - 150.);
    }

    #[test]
    fn a_picker_sits_above_its_trigger_and_shrinks_in_a_small_window() {
        let trigger = anchor(1040., 640., 28., 28.);
        let picker = fit(Place::Above(trigger), (352., 400.), WINDOW);
        assert_eq!(picker, Frame { x: 1068. - 352., y: 640. - 6. - 400., w: 352., h: 400. });
        // At 540 × 440 the composer's trigger is near the bottom: 400 does not fit above, so it
        // takes the room above less the margin and scrolls.
        let small = fit(Place::Above(anchor(480., 380., 28., 28.)), (352., 400.), (540., 440.));
        assert_eq!(small.h, 380. - 6. - 8.);
        assert_eq!(small.y, MARGIN);
        assert_eq!(small.x, 508. - 352.);
    }

    #[test]
    fn the_switcher_modal_and_toast_take_their_fixed_frames() {
        assert_eq!(switcher_frame(WINDOW, 36.), Frame { x: 280., y: 72., w: 560., h: 576. });
        // At the minimum window it is 492 wide and keeps 72 above and below.
        assert_eq!(switcher_frame((540., 440.), 36.), Frame { x: 24., y: 72., w: 492., h: 296. });
        assert_eq!(toast_width(1120.), 340.);
        assert_eq!(toast_width(300.), 268.);
        // A 440 modal in the smallest window still fits, 40 above center where there is room.
        assert_eq!(modal_top(200., 720.), 220.);
        assert_eq!(modal_top(430., 440.), MARGIN);
        let modal = fit(Place::Center, (440., 200.), (540., 440.));
        assert_eq!((modal.x, modal.y), (50., 80.));
    }

    #[test]
    fn a_submenu_or_rail_popover_opens_beside_and_flips() {
        let item = anchor(300., 100., 220., 28.);
        assert_eq!(fit(Place::Beside(item), (200., 120.), WINDOW).x, 526.);
        let right = fit(Place::Beside(anchor(900., 100., 200., 28.)), (200., 120.), WINDOW);
        assert_eq!(right.x, 900. - 6. - 200.);
        // A rail button near the bottom: tops aligned would leave; bottoms align instead.
        let low = fit(Place::Beside(anchor(16., 660., 40., 40.)), (240., 300.), WINDOW);
        assert_eq!(low.y, 700. - 300.);
    }
}
