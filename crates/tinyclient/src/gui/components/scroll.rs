//! Shared scroll controls. Geometry is read during paint, after the content has laid out.
use super::super::theme;
use gpui::{
    canvas, div, point, prelude::*, px, size, AnyElement, App, Bounds, Div, ElementId,
    HitboxBehavior, IntoElement, KeyDownEvent, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, RenderOnce, ScrollHandle, Stateful,
    UniformListScrollHandle, Window,
};

#[derive(Clone)]
pub enum ScrollTarget {
    Container(ScrollHandle),
    List(ListState),
}
impl From<ScrollHandle> for ScrollTarget {
    fn from(value: ScrollHandle) -> Self {
        Self::Container(value)
    }
}
impl From<ListState> for ScrollTarget {
    fn from(value: ListState) -> Self {
        Self::List(value)
    }
}
impl From<UniformListScrollHandle> for ScrollTarget {
    fn from(value: UniformListScrollHandle) -> Self {
        Self::Container(value.0.borrow().base_handle.clone())
    }
}
impl ScrollTarget {
    fn geometry(&self) -> (Point<Pixels>, Point<Pixels>, Bounds<Pixels>) {
        match self {
            Self::Container(h) => (h.offset(), h.max_offset(), h.bounds()),
            Self::List(h) => (
                h.scroll_px_offset_for_scrollbar(),
                h.max_offset_for_scrollbar(),
                h.viewport_bounds(),
            ),
        }
    }
    /// Page navigation for regions whose parent already owns keyboard focus.
    pub fn page_key(&self, event: &KeyDownEvent) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return false;
        }
        let (mut offset, max, bounds) = self.geometry();
        let page = bounds.size.height * 0.9;
        offset.y = match event.keystroke.key.as_str() {
            "pageup" => (offset.y + page).min(px(0.)),
            "pagedown" => (offset.y - page).max(-max.y),
            "home" => px(0.),
            "end" => -max.y,
            _ => return false,
        };
        self.set(offset);
        true
    }
    fn set(&self, offset: Point<Pixels>) {
        match self {
            Self::Container(h) => h.set_offset(offset),
            Self::List(h) => h.set_offset_from_scrollbar(offset),
        }
    }
    fn dragging(&self, active: bool) {
        if let Self::List(h) = self {
            if active {
                h.scrollbar_drag_started();
            } else {
                h.scrollbar_drag_ended();
            }
        }
    }
}

#[derive(Default)]
struct Pointer {
    grab: Option<(bool, f32)>,
    hovered: bool,
    drag_target: Option<ScrollTarget>,
}
impl Pointer {
    fn finish_drag(&mut self) {
        self.grab = None;
        if let Some(target) = self.drag_target.take() {
            target.dragging(false);
        }
    }
}
impl Drop for Pointer {
    fn drop(&mut self) {
        self.finish_drag();
    }
}

/// Wrap a bounded scrollable element. The content retains its existing scroll state.
#[derive(IntoElement)]
pub struct ScrollFrame {
    id: ElementId,
    content: AnyElement,
    target: ScrollTarget,
    natural_height: bool,
    keyboard: bool,
}
pub fn scroll_frame(
    id: impl Into<ElementId>,
    target: impl Into<ScrollTarget>,
    content: impl IntoElement,
) -> ScrollFrame {
    ScrollFrame {
        id: id.into(),
        target: target.into(),
        content: content.into_any_element(),
        natural_height: false,
        keyboard: true,
    }
}

impl ScrollFrame {
    pub fn fit_content(mut self) -> Self {
        self.natural_height = true;
        self
    }
    pub fn keyboard(mut self, enabled: bool) -> Self {
        self.keyboard = enabled;
        self
    }
}

/// Ordinary page with a handle scoped to this element's identity.
#[derive(IntoElement)]
pub struct ScrollPage {
    id: ElementId,
    content: Stateful<Div>,
    natural_height: bool,
}
pub fn scroll_page(id: impl Into<ElementId>, content: Stateful<Div>) -> ScrollPage {
    ScrollPage {
        id: id.into(),
        content,
        natural_height: false,
    }
}
/// A horizontally scrolling block whose height follows its content.
pub fn scroll_content(id: impl Into<ElementId>, content: Stateful<Div>) -> ScrollPage {
    ScrollPage {
        id: id.into(),
        content,
        natural_height: true,
    }
}
impl RenderOnce for ScrollPage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let handle = window
            .use_keyed_state((self.id.clone(), "page-scroll"), cx, |_, _| {
                ScrollHandle::new()
            })
            .read(cx)
            .clone();
        let content = self
            .content
            .when(!self.natural_height, |content| content.size_full());
        let mut frame = scroll_frame(self.id, handle.clone(), content.track_scroll(&handle));
        frame.natural_height = self.natural_height;
        frame
    }
}

/// Thumb position and length in the usable track, with a usable minimum grab size.
fn thumb(track: f32, viewport: f32, max: f32, offset: f32) -> (f32, f32) {
    // Leave some travel even in a very short viewport or with only slight overflow.
    let longest = if max > 0. {
        (track - 4.).max(track / 2.)
    } else {
        track
    };
    let length = (track * viewport / (viewport + max)).max(32.).min(longest);
    let position = if max > 0. {
        (-offset / max).clamp(0., 1.) * (track - length)
    } else {
        0.
    };
    (position, length)
}

impl RenderOnce for ScrollFrame {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pointer = window.use_keyed_state((self.id.clone(), "scroll-pointer"), cx, |_, _| {
            Pointer::default()
        });
        let focus = window
            .use_keyed_state((self.id.clone(), "scroll-focus"), cx, |_, cx| {
                cx.focus_handle().tab_index(0).tab_stop(true)
            })
            .read(cx)
            .clone();
        let target = self.target;
        let keyboard = target.clone();
        let key_focus = focus.clone();
        let bar_focus = self.keyboard.then_some(focus.clone());
        let click_focus = focus.clone();
        let bars = canvas(
            move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                let (offset, max, viewport) = target.geometry();
                let mut tracks = Vec::new();
                for horizontal in [false, true] {
                    let (extent, page, limit, current) = if horizontal {
                        (bounds.size.width, viewport.size.width, max.x, offset.x)
                    } else {
                        (bounds.size.height, viewport.size.height, max.y, offset.y)
                    };
                    if limit <= px(0.) || extent <= px(8.) {
                        continue;
                    }
                    let track_len = f32::from(extent)
                        - 6.
                        - if horizontal || max.x > px(0.) {
                            14.
                        } else {
                            0.
                        };
                    if track_len <= 0. {
                        continue;
                    }
                    let (at, length) = thumb(track_len, page.into(), limit.into(), current.into());
                    let track = if horizontal {
                        Bounds::new(
                            point(bounds.left() + px(3.), bounds.bottom() - px(14.)),
                            size(px(track_len), px(14.)),
                        )
                    } else {
                        Bounds::new(
                            point(bounds.right() - px(14.), bounds.top() + px(3.)),
                            size(px(14.), px(track_len)),
                        )
                    };
                    let active = pointer.read(cx).hovered || pointer.read(cx).grab.is_some();
                    let width = if active { 8. } else { 6. };
                    let painted = if horizontal {
                        Bounds::new(
                            point(track.left() + px(at), track.top() + px((14. - width) / 2.)),
                            size(px(length), px(width)),
                        )
                    } else {
                        Bounds::new(
                            point(track.left() + px((14. - width) / 2.), track.top() + px(at)),
                            size(px(width), px(length)),
                        )
                    };
                    window.paint_quad(
                        PaintQuad::new(painted)
                            .background(if active {
                                theme::scrollbar_hover()
                            } else {
                                theme::scrollbar()
                            })
                            .corner_radii(px(3.)),
                    );
                    tracks.push((
                        horizontal,
                        track,
                        at,
                        length,
                        f32::from(limit),
                        f32::from(page),
                    ));
                }
                let down_target = target.clone();
                let down_pointer = pointer.clone();
                let down_tracks = tracks.clone();
                let down_focus = bar_focus.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble()
                        || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    for &(horizontal, track, at, length, limit, page) in &down_tracks {
                        if !track.contains(&event.position) {
                            continue;
                        }
                        let position = f32::from(if horizontal {
                            event.position.x - track.left()
                        } else {
                            event.position.y - track.top()
                        });
                        if position >= at && position <= at + length {
                            down_target.dragging(true);
                            window.capture_pointer(hitbox.id);
                            down_pointer.update(cx, |state, _| {
                                state.grab = Some((horizontal, position - at));
                                state.drag_target = Some(down_target.clone());
                            });
                        } else {
                            let (mut offset, _, _) = down_target.geometry();
                            let value = if horizontal {
                                &mut offset.x
                            } else {
                                &mut offset.y
                            };
                            *value = px((f32::from(*value)
                                + if position < at {
                                    page * 0.9
                                } else {
                                    -page * 0.9
                                })
                            .clamp(-limit, 0.));
                            down_target.set(offset);
                        }
                        // Thumb and track presses scroll; they must not steal focus onto the
                        // frame. Keyboard focus stays on Tab / programmatic focus only.
                        if scrollbar_press_focuses_frame() {
                            if let Some(focus) = &down_focus {
                                window.focus(focus, cx);
                            }
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                        window.refresh();
                        break;
                    }
                });
                let move_target = target.clone();
                let move_pointer = pointer.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.capture() {
                        return;
                    }
                    let hovered = tracks
                        .iter()
                        .any(|(_, track, ..)| track.contains(&event.position));
                    let grab = move_pointer.read(cx).grab;
                    if move_pointer.read(cx).hovered != hovered {
                        move_pointer.update(cx, |state, _| state.hovered = hovered);
                        window.refresh();
                    }
                    if let Some((axis, grab)) = grab {
                        if event.pressed_button != Some(MouseButton::Left) {
                            move_pointer.update(cx, |state, _| state.finish_drag());
                            window.release_pointer();
                            window.refresh();
                            return;
                        }
                        for &(horizontal, track, _, length, limit, _) in &tracks {
                            if horizontal != axis {
                                continue;
                            }
                            let (position, extent) = if horizontal {
                                (event.position.x - track.left(), track.size.width)
                            } else {
                                (event.position.y - track.top(), track.size.height)
                            };
                            let travel = f32::from(extent) - length;
                            if travel <= 0. {
                                continue;
                            }
                            let value =
                                -((f32::from(position) - grab) / travel).clamp(0., 1.) * limit;
                            let (mut offset, _, _) = move_target.geometry();
                            if horizontal {
                                offset.x = px(value);
                            } else {
                                offset.y = px(value);
                            }
                            move_target.set(offset);
                            window.refresh();
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase.capture()
                        && event.button == MouseButton::Left
                        && pointer.read(cx).grab.is_some()
                    {
                        pointer.update(cx, |state, _| state.finish_drag());
                        // capture_pointer on thumb-down must clear here. The move path
                        // releases when the button is already up, but a mouse-up with no
                        // further move leaves the scroll hitbox owning every later click.
                        if scrollbar_mouseup_releases_pointer(true) {
                            window.release_pointer();
                        }
                        window.refresh();
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        div()
            .id(self.id)
            .relative()
            .w_full()
            .when(!self.natural_height, |frame| frame.h_full())
            .min_h_0()
            .min_w_0()
            .when(self.keyboard, |frame| {
                frame
                    .track_focus(&focus)
                    .tab_index(0)
                    .focus_visible(|style| style.shadow(frame_focus_ring()))
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                // GPUI's track_focus registers an earlier bubble listener that focuses
                // this frame on every press unless default is prevented. Call that here
                // (still before the auto-focus listener in reverse registration order)
                // so empty chrome does not steal focus from controls.
                if mouse_press_focuses_scroll_frame(
                    self.keyboard,
                    click_focus.contains_focused(window, cx),
                ) {
                    window.focus(&click_focus, cx);
                } else {
                    window.prevent_default();
                }
            })
            .on_key_down(move |event, window, cx| {
                // Descendant text inputs and controls retain their editing keys.
                if !self.keyboard
                    || !key_focus.is_focused(window)
                    || event.keystroke.modifiers.control
                    || event.keystroke.modifiers.platform
                    || event.keystroke.modifiers.alt
                {
                    return;
                }
                if keyboard.page_key(event) {
                    cx.stop_propagation();
                    window.refresh();
                    return;
                }
                let (mut offset, max, _) = keyboard.geometry();
                let key = event.keystroke.key.as_str();
                if matches!(key, "left" | "right") && max.x > px(0.) {
                    offset.x = (offset.x + px(if key == "left" { 40. } else { -40. }))
                        .clamp(-max.x, px(0.));
                    keyboard.set(offset);
                    cx.stop_propagation();
                    window.refresh();
                    return;
                }
                offset.y = match key {
                    "up" => (offset.y + px(40.)).min(px(0.)),
                    "down" => (offset.y - px(40.)).max(-max.y),
                    _ => return,
                };
                keyboard.set(offset);
                cx.stop_propagation();
                window.refresh();
            })
            .child(self.content)
            .child(bars)
    }
}

/// Keyboard focus on a scroll frame. An outset ring is painted under the whole frame, and a
/// transparent frame shows that as a fill.
pub(crate) fn frame_focus_ring() -> Vec<gpui::BoxShadow> {
    theme::focus_ring_inset()
}

/// Whether mouse-up after a scrollbar thumb drag must release the captured pointer.
/// A finished grab without release leaves later clicks on the scroll hitbox.
pub(crate) fn scrollbar_mouseup_releases_pointer(had_grab: bool) -> bool {
    had_grab
}

/// Whether a left press on empty scroll chrome should move focus onto the frame.
///
/// GPUI's `track_focus` auto-focuses on mouse press. Allowing that on scroll frames
/// made every keyboard-enabled pane look "active" (blue inset ring once any key set
/// keyboard modality) and stole focus from controls, so later clicks felt dead. When
/// this returns false the frame's mouse-down handler calls `prevent_default` so the
/// auto-focus transfer is skipped. Tab still focuses the frame for page keys;
/// `focus_visible` keeps the ring keyboard-only.
pub(crate) fn mouse_press_focuses_scroll_frame(keyboard: bool, contains_focused: bool) -> bool {
    let _ = (keyboard, contains_focused);
    false
}

/// Whether a scrollbar thumb or track press should focus the scroll frame.
pub(crate) fn scrollbar_press_focuses_frame() -> bool {
    false
}

/// The lab's scroll-release driver. A thumb drag's mouse-up releases the pointer.
pub(crate) fn prove_scroll_release() -> Result<(), &'static str> {
    if scrollbar_mouseup_releases_pointer(true) && !scrollbar_mouseup_releases_pointer(false) {
        Ok(())
    } else {
        Err("a scrollbar mouse-up would leave the pointer captured")
    }
}

/// The lab's pane-focus-pointer driver. A mouse press on scroll chrome must not
/// move focus onto the frame (Tab still can; the ring stays keyboard-only).
pub(crate) fn prove_pane_focus_pointer() -> Result<(), &'static str> {
    if mouse_press_focuses_scroll_frame(true, false) {
        return Err("a mouse press on scroll chrome still moves focus onto the frame");
    }
    if mouse_press_focuses_scroll_frame(true, true) {
        return Err("a mouse press focuses the frame even when a descendant is focused");
    }
    if mouse_press_focuses_scroll_frame(false, false) {
        return Err("a non-keyboard scroll frame takes focus from a mouse press");
    }
    if scrollbar_press_focuses_frame() {
        return Err("a scrollbar press still moves focus onto the frame");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        frame_focus_ring, mouse_press_focuses_scroll_frame, scrollbar_mouseup_releases_pointer,
        scrollbar_press_focuses_frame, thumb,
    };
    #[test]
    fn thumb_tracks_endpoints_and_clamps() {
        assert_eq!(thumb(200., 200., 600., 0.), (0., 50.));
        assert_eq!(thumb(200., 200., 600., -600.), (150., 50.));
        assert_eq!(thumb(200., 200., 600., -900.), (150., 50.));
        assert_eq!(thumb(200., 200., 10000., 0.).1, 32.);
        assert_eq!(thumb(20., 20., 1000., 0.), (0., 16.));
        assert_eq!(thumb(20., 20., 1000., -1000.), (4., 16.));
    }

    #[test]
    fn a_keyboard_scroll_frame_focus_ring_does_not_fill_the_frame() {
        assert!(
            frame_focus_ring().iter().all(|shadow| shadow.inset),
            "an outset shadow is painted under a transparent frame"
        );
    }

    #[test]
    fn a_scrollbar_mouseup_releases_the_captured_pointer() {
        assert!(scrollbar_mouseup_releases_pointer(true));
        assert!(!scrollbar_mouseup_releases_pointer(false));
    }

    #[test]
    fn a_mouse_press_does_not_focus_the_scroll_frame() {
        assert!(
            !mouse_press_focuses_scroll_frame(true, false),
            "empty chrome must not steal focus on mouse press"
        );
        assert!(!mouse_press_focuses_scroll_frame(true, true));
        assert!(!mouse_press_focuses_scroll_frame(false, false));
        assert!(
            !scrollbar_press_focuses_frame(),
            "a scrollbar press must not focus the frame"
        );
    }
}

#[cfg(all(test, feature = "gui-shot"))]
mod interaction_tests {
    use super::*;
    use gpui::{list, Context, FocusHandle, ListAlignment, Render, TestApp};
    use std::{cell::Cell, rc::Rc};

    struct Page {
        scroll: ScrollHandle,
        clicks: Rc<Cell<usize>>,
        control: FocusHandle,
        keys: usize,
    }
    impl Render for Page {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            scroll_frame(
                "test-page",
                self.scroll.clone(),
                div()
                    .id("content")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("control")
                            .h(px(40.))
                            .flex_none()
                            .track_focus(&self.control)
                            .on_key_down(cx.listener(|this, _, _, cx| {
                                this.keys += 1;
                                cx.stop_propagation();
                            }))
                            .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
                    )
                    .children((1..50).map(|ix| div().id(ix).h(px(40.)).flex_none())),
            )
        }
    }

    #[test]
    fn track_drag_wheel_and_child_clicks() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, cx| Page {
            scroll: ScrollHandle::new(),
            clicks: Rc::new(Cell::new(0)),
            control: cx.focus_handle(),
            keys: 0,
        });
        window.draw();
        let bounds = window.read(|page, _| page.scroll.bounds());
        let x = bounds.right() - px(7.);
        window.simulate_click(point(x, bounds.bottom() - px(25.)), MouseButton::Left);
        window.draw();
        window.read(|page, _| assert!(page.scroll.offset().y < px(0.)));
        window.update(|page, _, _| page.scroll.set_offset(point(px(0.), px(0.))));
        window.draw();
        window.simulate_mouse_down(point(x, bounds.top() + px(10.)), MouseButton::Left);
        let end = point(x, bounds.bottom() - px(3.));
        window.simulate_event(MouseMoveEvent {
            position: end,
            modifiers: Default::default(),
            pressed_button: Some(MouseButton::Left),
        });
        window.draw();
        window.simulate_mouse_up(end, MouseButton::Left);
        window.draw();
        window.read(|page, _| {
            assert!((page.scroll.offset().y + page.scroll.max_offset().y).abs() < px(1.))
        });
        window.update(|page, _, _| page.scroll.set_offset(point(px(0.), px(0.))));
        window.draw();
        window.simulate_click(bounds.origin + point(px(20.), px(20.)), MouseButton::Left);
        window.read(|page, _| assert_eq!(page.clicks.get(), 1));
        window.simulate_scroll(
            bounds.origin + point(px(30.), px(30.)),
            point(px(0.), px(-120.)),
        );
        window.draw();
        window.read(|page, _| assert!(page.scroll.offset().y < px(0.)));
    }

    #[test]
    fn a_mouse_click_keeps_focus_off_the_scroll_frame_and_clicks_still_work() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, cx| Page {
            scroll: ScrollHandle::new(),
            clicks: Rc::new(Cell::new(0)),
            control: cx.focus_handle(),
            keys: 0,
        });
        window.draw();
        window.update(|page, window, cx| window.focus(&page.control, cx));
        window.draw();
        window.update(|page, window, _| assert!(page.control.is_focused(window)));
        let bounds = window.read(|page, _| page.scroll.bounds());
        // Empty chrome below the control: must not steal focus onto the frame.
        window.simulate_click(
            bounds.origin + point(px(20.), px(80.)),
            MouseButton::Left,
        );
        window.draw();
        window.update(|page, window, _| {
            assert!(
                page.control.is_focused(window),
                "a mouse press on scroll chrome stole focus from the control"
            );
        });
        window.simulate_click(bounds.origin + point(px(20.), px(20.)), MouseButton::Left);
        window.read(|page, _| assert_eq!(page.clicks.get(), 1));
    }

    #[test]
    fn keyboard_scrolls_focused_page_and_preserves_control_keys() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, cx| Page {
            scroll: ScrollHandle::new(),
            clicks: Rc::new(Cell::new(0)),
            control: cx.focus_handle(),
            keys: 0,
        });
        window.draw();
        window.update(|_, window, cx| window.focus_next(cx));
        window.simulate_keystroke("pagedown");
        window.draw();
        window.read(|page, _| assert!(page.scroll.offset().y < px(0.)));
        window.simulate_keystroke("end");
        window.draw();
        window.read(|page, _| {
            assert!((page.scroll.offset().y + page.scroll.max_offset().y).abs() < px(1.))
        });
        window.simulate_keystroke("home");
        window.draw();
        window.read(|page, _| assert_eq!(page.scroll.offset().y, px(0.)));
        window.update(|page, window, cx| window.focus(&page.control, cx));
        window.simulate_keystroke("pagedown");
        window.draw();
        window.read(|page, _| {
            assert_eq!(page.keys, 1);
            assert_eq!(page.scroll.offset().y, px(0.));
        });
    }
    struct VirtualPage {
        list: ListState,
    }
    impl Render for VirtualPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            scroll_frame(
                "virtual-page",
                self.list.clone(),
                list(self.list.clone(), |ix, _, _| {
                    div().id(ix).h(px(40.)).into_any_element()
                })
                .size_full(),
            )
            .keyboard(false)
        }
    }

    #[test]
    fn virtual_list_freezes_geometry_and_ends_drag_on_release() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| VirtualPage {
            list: ListState::new(100, ListAlignment::Top, px(200.)).measure_all(),
        });
        window.draw();
        let bounds = window.read(|page, _| page.list.viewport_bounds());
        let x = bounds.right() - px(7.);
        window.simulate_mouse_down(point(x, bounds.top() + px(10.)), MouseButton::Left);
        window.read(|page, _| assert!(page.list.is_scrollbar_dragging()));
        let end = point(x, bounds.bottom() - px(3.));
        window.simulate_event(MouseMoveEvent {
            position: end,
            modifiers: Default::default(),
            pressed_button: Some(MouseButton::Left),
        });
        window.draw();
        window.read(|page, _| assert!(page.list.logical_scroll_top().item_ix > 0));
        window.simulate_mouse_up(end, MouseButton::Left);
        window.draw();
        window.read(|page, _| assert!(!page.list.is_scrollbar_dragging()));
    }

    #[test]
    fn bottom_aligned_history_can_leave_and_return_to_latest() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| VirtualPage {
            list: ListState::new(100, ListAlignment::Bottom, px(200.)).measure_all(),
        });
        window.draw();
        window.read(|page, _| assert_eq!(page.list.is_scrolled_to_end(), Some(true)));
        let bounds = window.read(|page, _| page.list.viewport_bounds());
        let x = bounds.right() - px(7.);
        let top = point(x, bounds.top() + px(3.));
        window.simulate_mouse_down(point(x, bounds.bottom() - px(10.)), MouseButton::Left);
        window.simulate_event(MouseMoveEvent {
            position: top,
            modifiers: Default::default(),
            pressed_button: Some(MouseButton::Left),
        });
        window.draw();
        window.simulate_mouse_up(top, MouseButton::Left);
        window.draw();
        window.read(|page, _| assert_eq!(page.list.is_scrolled_to_end(), Some(false)));
        window.simulate_mouse_down(point(x, bounds.top() + px(10.)), MouseButton::Left);
        let end = point(x, bounds.bottom() - px(3.));
        window.simulate_event(MouseMoveEvent {
            position: end,
            modifiers: Default::default(),
            pressed_button: Some(MouseButton::Left),
        });
        window.draw();
        window.simulate_mouse_up(end, MouseButton::Left);
        window.draw();
        window.read(|page, _| assert_eq!(page.list.is_scrolled_to_end(), Some(true)));
    }

    struct HorizontalPage {
        scroll: ScrollHandle,
    }
    impl Render for HorizontalPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            scroll_frame(
                "horizontal-page",
                self.scroll.clone(),
                div()
                    .id("horizontal-content")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().w(px(4000.)).h(px(40.))),
            )
        }
    }

    #[test]
    fn horizontal_track_and_thumb_reach_the_end() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| HorizontalPage {
            scroll: ScrollHandle::new(),
        });
        window.draw();
        let bounds = window.read(|page, _| page.scroll.bounds());
        let y = bounds.bottom() - px(7.);
        window.simulate_click(point(bounds.right() - px(25.), y), MouseButton::Left);
        window.draw();
        window.read(|page, _| assert!(page.scroll.offset().x < px(0.)));
        window.update(|page, _, _| page.scroll.set_offset(point(px(0.), px(0.))));
        window.draw();
        window.simulate_mouse_down(point(bounds.left() + px(10.), y), MouseButton::Left);
        let end = point(bounds.right() - px(3.), y);
        window.simulate_event(MouseMoveEvent {
            position: end,
            modifiers: Default::default(),
            pressed_button: Some(MouseButton::Left),
        });
        window.draw();
        window.simulate_mouse_up(end, MouseButton::Left);
        window.draw();
        window.read(|page, _| {
            assert!((page.scroll.offset().x + page.scroll.max_offset().x).abs() < px(1.))
        });
    }
    struct ContentPage {
        scroll: ScrollHandle,
    }
    impl Render for ContentPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    scroll_frame(
                        "natural-content",
                        self.scroll.clone(),
                        div()
                            .id("code-content")
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll)
                            .pb(px(16.))
                            .child(div().w(px(4000.)).h(px(80.))),
                    )
                    .fit_content(),
                )
                .child(div().h(px(40.)).flex_none())
        }
    }

    #[test]
    fn horizontal_content_keeps_its_natural_height() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| ContentPage {
            scroll: ScrollHandle::new(),
        });
        window.draw();
        window.read(|page, _| {
            assert_eq!(page.scroll.bounds().size.height, px(96.));
            assert!(page.scroll.max_offset().x > px(0.));
            assert_eq!(page.scroll.max_offset().y, px(0.));
        });
    }
}
