//! One block of styled text as an element: `StyledText` with what it cannot do itself painted
//! around it. Rounded backgrounds go behind the glyphs, custom emoji over their slots, and the
//! pointer finds links, mentions and spoilers through the same layout.

use super::rich::Inline;
use super::tokens;
use crate::gui::theme;
use gpui::{
    fill, font, point, prelude::*, px, quad, size, App, BorderStyle, Bounds, Corners, CursorStyle,
    DispatchPhase, ElementId, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    RenderImage, StrikethroughStyle, StyledText, TextLayout, TextRun, UnderlineStyle, Window,
};
use protocol::MentionKind;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// What the pointer did to a block of text.
pub enum TextEvent {
    /// The pointer is over this run, or over none of the ones that act.
    Hover(Option<usize>),
    /// A click on this run.
    Click(usize),
    /// The button went down on this byte of the shown text.
    Down(usize),
    /// The pointer is at this byte with the button held, in this block or one dragged into.
    Drag(usize),
}

/// Finds the decoded image of a custom emoji.
pub type Decoded<'a> = &'a mut dyn FnMut(&protocol::MediaFile) -> Found;
/// A decoded image, when there is one.
pub type Found = Option<Arc<RenderImage>>;

/// What a block is drawn with, beyond its runs.
pub struct Look<'a> {
    pub color: Hsla,
    /// The run under the pointer, if it acts.
    pub hovered: Option<usize>,
    /// Spoilers the user has opened, by the span's start.
    pub revealed: &'a dyn Fn(u32) -> bool,
    /// My own id on this account: a mention of it is drawn stronger.
    pub me: &'a str,
    /// The message as a whole mentions me, which is what makes `@everyone` mine.
    pub mentions_me: bool,
    /// The image of a custom emoji, once decoded.
    pub emoji: Decoded<'a>,
    pub selection: Option<Range<usize>>,
    /// A drag that began in another block of this message is under way.
    pub dragging: bool,
}

type Handler = Rc<dyn Fn(TextEvent, &mut Window, &mut App)>;

pub struct Rich {
    id: ElementId,
    text: StyledText,
    /// Rounded backgrounds, behind the glyphs.
    boxes: Vec<(Range<usize>, Hsla)>,
    images: Vec<(Range<usize>, Arc<RenderImage>)>,
    /// The runs that act on a click, by run index.
    hot: Vec<(Range<usize>, usize)>,
    selection: Option<Range<usize>>,
    dragging: bool,
    /// The shown text, for telling a line that holds only a box's padding.
    shown: gpui::SharedString,
    handler: Handler,
}

#[derive(Default, Clone)]
struct Pointer {
    /// Where the button went down, while it is down.
    down: Rc<Cell<Option<usize>>>,
    dragged: Rc<Cell<bool>>,
    hovered: Rc<Cell<Option<usize>>>,
}

impl Rich {
    pub fn new(
        id: impl Into<ElementId>,
        inline: &Inline,
        look: Look,
        handler: impl Fn(TextEvent, &mut Window, &mut App) + 'static,
    ) -> Rich {
        let mut runs = Vec::with_capacity(inline.runs.len());
        let mut boxes = Vec::new();
        let mut images = Vec::new();
        let mut hot = Vec::new();
        for (ix, (range, run)) in inline.ranges().enumerate() {
            let mut face = font(if run.code { tokens::MONO } else { theme::FONT });
            let mut color = look.color;
            let mut underline = run.underline;
            if run.bold {
                face.weight = FontWeight::SEMIBOLD;
            }
            if run.italic {
                face.style = FontStyle::Italic;
            }
            if run.strike || run.emoji.is_some() {
                color = theme::text_soft().into();
            }
            if run.mark {
                color = theme::text_faint().into();
            }
            if run.code {
                boxes.push((range.clone(), tokens::code_inline().into()));
            }
            if run.link.is_some() {
                color = tokens::link().into();
                underline |= look.hovered == Some(ix);
                hot.push((range.clone(), ix));
            }
            if let Some((kind, id)) = &run.mention {
                let mine = match kind {
                    MentionKind::User => !look.me.is_empty() && id == look.me,
                    MentionKind::Everyone | MentionKind::Role => look.mentions_me,
                    MentionKind::Channel => false,
                };
                face.weight = FontWeight::MEDIUM;
                color = if mine { gpui::white() } else { tokens::link().into() };
                let fill: Hsla = if mine { tokens::mention_me().into() } else { theme::accent_soft().into() };
                boxes.push((range.clone(), fill));
                hot.push((range.clone(), ix));
            }
            if let Some(spoiler) = run.spoiler {
                if (look.revealed)(spoiler) {
                    boxes.push((range.clone(), tokens::code_inline().into()));
                } else {
                    // Hidden text is painted in the color of its cover, so nothing shows through.
                    color = tokens::spoiler().into();
                    underline = false;
                    boxes.push((range.clone(), tokens::spoiler().into()));
                    hot.retain(|(_, run)| *run != ix);
                    hot.push((range.clone(), ix));
                }
            }
            if let Some(image) = run.emoji.as_ref().filter(|file| file.path.is_some()).and_then(&mut *look.emoji) {
                images.push((range.clone(), image));
            }
            runs.push(TextRun {
                len: run.len,
                font: face,
                color,
                background_color: None,
                underline: underline.then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: run.strike.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            });
        }
        let shown: gpui::SharedString = inline.text.clone().into();
        Rich {
            id: id.into(),
            text: StyledText::new(shown.clone()).with_runs(runs),
            dragging: look.dragging,
            shown,
            boxes,
            images,
            hot,
            selection: look.selection.map(|range| range.start.min(inline.text.len())..range.end.min(inline.text.len())),
            handler: Rc::new(handler),
        }
    }
}

/// The byte under `position`, or the nearest one when it is past the end of a line.
fn index_at(layout: &TextLayout, position: gpui::Point<Pixels>) -> usize {
    match layout.index_for_position(position) {
        Ok(ix) | Err(ix) => ix,
    }
}

/// The box of `range` on each visual line it touches.
pub fn segments(layout: &TextLayout, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
    parts(layout, range).into_iter().map(|(_, bounds)| bounds).collect()
}

/// The same, with the bytes each box holds.
fn parts(layout: &TextLayout, range: &Range<usize>) -> Vec<(Range<usize>, Bounds<Pixels>)> {
    let origin = layout.bounds().origin;
    let line_height = layout.line_height();
    let mut found = Vec::new();
    let mut line_start = 0;
    let mut y = origin.y;
    for line in layout.line_layouts() {
        let unwrapped = &line.unwrapped_layout;
        // A boundary names the glyph that starts the next visual line.
        let breaks = line.wrap_boundaries().iter().map(|at| line.runs()[at.run_ix].glyphs[at.glyph_ix].index);
        let mut row_start = 0;
        for row_end in breaks.chain([line.len()]) {
            let start = range.start.max(line_start + row_start);
            let end = range.end.min(line_start + row_end);
            if start < end {
                let row_x = unwrapped.x_for_index(row_start);
                let left = unwrapped.x_for_index(start - line_start) - row_x;
                let right = unwrapped.x_for_index(end - line_start) - row_x;
                found.push((start..end, Bounds::new(point(origin.x + left, y), size(right - left, line_height))));
            }
            row_start = row_end;
            y += line_height;
        }
        line_start += line.len() + 1;
    }
    found
}

impl IntoElement for Rich {
    type Element = Rich;

    fn into_element(self) -> Rich {
        self
    }
}

impl Element for Rich {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text.prepaint(None, inspector, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        for (range, color) in &self.boxes {
            for (bytes, part) in parts(&layout, range) {
                // A box that wraps leaves its trailing padding alone on the next line: no pill.
                if self.shown.get(bytes).is_none_or(super::rich::is_padding) {
                    continue;
                }
                // Two points clear of the line above and below, so stacked lines do not merge.
                let part = Bounds::new(part.origin + point(px(0.), px(2.)), size(part.size.width, part.size.height - px(4.)));
                window.paint_quad(quad(part, px(4.), *color, px(0.), gpui::transparent_black(), BorderStyle::default()));
            }
        }
        if let Some(selection) = &self.selection {
            for part in segments(&layout, selection) {
                window.paint_quad(fill(part, tokens::selection()));
            }
        }
        self.text.paint(None, inspector, bounds, &mut (), &mut (), window, cx);
        for (range, image) in &self.images {
            let Some(slot) = segments(&layout, range).into_iter().next() else {
                continue;
            };
            let side = px(18.);
            let at = Bounds::new(
                point(slot.center().x - side / 2., slot.center().y - side / 2.),
                size(side, side),
            );
            let _ = window.paint_image(at, at, Corners::default(), image.clone(), 0, false);
        }

        let over = |position, layout: &TextLayout, hot: &[(Range<usize>, usize)]| {
            let ix = layout.index_for_position(position).ok()?;
            hot.iter().find(|(range, _)| range.contains(&ix)).map(|(_, run)| *run)
        };
        if hitbox.is_hovered(window) {
            let acts = over(window.mouse_position(), &layout, &self.hot).is_some();
            let style = if acts { CursorStyle::PointingHand } else { CursorStyle::IBeam };
            window.set_cursor_style(style, hitbox);
        }

        let Some(global_id) = global_id else {
            return;
        };
        let pointer = window.with_element_state::<Pointer, _>(global_id, |pointer, _| {
            let pointer = pointer.unwrap_or_default();
            (pointer.clone(), pointer)
        });
        let hot: Rc<[(Range<usize>, usize)]> = std::mem::take(&mut self.hot).into();

        window.on_mouse_event({
            let (pointer, hitbox, layout, handler) = (pointer.clone(), hitbox.clone(), layout.clone(), self.handler.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                    let at = index_at(&layout, event.position);
                    pointer.down.set(Some(at));
                    pointer.dragged.set(false);
                    handler(TextEvent::Down(at), window, cx);
                }
            }
        });
        window.on_mouse_event({
            let (pointer, hitbox, layout) = (pointer.clone(), hitbox.clone(), layout.clone());
            let (hot, handler, dragging) = (hot.clone(), self.handler.clone(), self.dragging);
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if event.pressed_button == Some(MouseButton::Left) && (pointer.down.get().is_some() || dragging) {
                    // The block the pointer is level with speaks, so a drag runs on into the
                    // next block and back.
                    let level = (bounds.top()..bounds.bottom()).contains(&event.position.y);
                    let head = index_at(&layout, event.position);
                    let moved = pointer.down.get().is_none_or(|anchor| head != anchor) || pointer.dragged.get();
                    if level && moved {
                        pointer.dragged.set(true);
                        handler(TextEvent::Drag(head), window, cx);
                    }
                    return;
                }
                let now = hitbox.is_hovered(window).then(|| over(event.position, &layout, &hot)).flatten();
                if pointer.hovered.replace(now) != now {
                    handler(TextEvent::Hover(now), window, cx);
                }
            }
        });
        window.on_mouse_event({
            let (hitbox, handler) = (hitbox.clone(), self.handler.clone());
            move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                let Some(anchor) = pointer.down.take() else {
                    return;
                };
                if pointer.dragged.get() || !hitbox.is_hovered(window) {
                    return;
                }
                // A click is a press and a release on the same run.
                let run = over(event.position, &layout, &hot);
                let pressed = hot.iter().find(|(range, _)| range.contains(&anchor)).map(|(_, run)| *run);
                if let (Some(run), true) = (run, run == pressed) {
                    handler(TextEvent::Click(run), window, cx);
                }
            }
        });
    }
}
