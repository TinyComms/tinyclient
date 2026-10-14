//! The chips above the field: a picture's thumbnail or a file's tile, its name and size, why it
//! is refused, the tags of what is set on it, remove, and the options it has. Thumbnails are
//! decoded off the main thread at 128 px; the tray holds paths, never bytes.

use super::attach::{self, Kind, Options, Staged};
use super::picker::images::{Loaded, Picture};
use super::Composer;
use crate::gui::components::{glyph, menu, menu_item};
use crate::gui::theme::{self, Type};
use gpui::{anchored, deferred, Anchor, div, img, point, prelude::*, px, rgba, AnyElement, Context, Div, ImageSource, MouseButton, Rgba, Window};

const THUMB: f32 = 64.;
const CONTROL: f32 = 20.;

/// Black at 45% over a picture being uploaded or marked spoiler, and 60% under a tag.
fn media_dim() -> Rgba {
    rgba(0x00000073)
}

fn media_tag() -> Rgba {
    rgba(0x00000099)
}

/// "Trail map…autumn.pdf": the start and the end, so the extension stays.
pub fn middle(name: &str, most: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= most {
        return name.to_owned();
    }
    let tail = (most / 3).max(4);
    let head = most.saturating_sub(tail + 1);
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{}…{end}", start.trim_end())
}

fn file_icon(kind: Kind) -> &'static str {
    // CONTRACT: the design's typed file glyphs (file-text, file-archive, …) are not among slice
    // A's assets; pictures, video and audio use the glyphs there are.
    match kind {
        Kind::Image => "icons/image.svg",
        Kind::Video => "icons/video.svg",
        Kind::Audio => "icons/volume-2.svg",
        Kind::Archive | Kind::Code | Kind::Text | Kind::Other => "icons/file.svg",
    }
}

fn corner(id: (&'static str, u64), icon: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size(px(CONTROL))
        .flex_none()
        .rounded(px(theme::R_CONTROL))
        .bg(theme::overlay())
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme::tooltip()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph(icon, 12., theme::text_soft()))
}

fn tag(words: &'static str) -> Div {
    div()
        .h(px(16.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .bg(media_tag())
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(theme::MEDIUM)
        .text_color(theme::on_accent())
        .child(words)
}

fn tags(chip: &Staged) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if chip.spoiler {
        tags.push("Spoiler");
    }
    if chip.as_file {
        tags.push("File");
    }
    if chip.sends_jpeg() {
        tags.push("JPEG");
    }
    tags
}

impl Composer {
    pub(super) fn thumb_decoded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        if self.thumbs.store(loaded) {
            cx.notify();
        }
        self.thumbs.pump(cx, Self::thumb_decoded);
    }

    /// The row of chips, inside the card above the field.
    pub(super) fn tray(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        if self.tray.is_empty() {
            return None;
        }
        self.thumbs.begin_frame();
        let (can, network) = self.target.as_ref().map(|target| (target.can, target.key.network))?;
        let edge = (THUMB * window.scale_factor()).ceil() as u32;
        let chips: Vec<Staged> = self.tray.clone();
        let mut row = div().id("tray").px(px(8.)).pt(px(8.)).flex().gap(px(8.)).overflow_x_scroll();
        for chip in chips {
            let options = attach::options(&chip, &can, network);
            let picture = match (&chip.converted, chip.pictured()) {
                (_, false) => None,
                (Some(jpeg), true) => Some(jpeg.to_string_lossy().into_owned()),
                (None, true) if chip.converting => None,
                (None, true) => Some(chip.path.to_string_lossy().into_owned()),
            };
            let decoded = picture.map(|path| self.thumbs.get(&path, edge, Some(1.), cx, Self::thumb_decoded));
            let element = match decoded {
                Some(picture) => self.picture_chip(&chip, picture, options, cx),
                None => self.file_chip(&chip, options, cx),
            };
            row = row.child(element);
        }
        Some(div().child(row))
    }

    fn chip_controls(&self, chip: &Staged, options: Options, cx: &mut Context<Self>) -> Div {
        let id = chip.id;
        let remove = corner(("chip-remove", id), "icons/x.svg").on_click(cx.listener(move |this, _, _, cx| this.remove_chip(id, cx)));
        div()
            .absolute()
            .top(px(4.))
            .right(px(4.))
            .flex()
            .gap(px(4.))
            .when(options.any(), |controls| {
                controls.child(corner(("chip-options", id), "icons/ellipsis.svg").on_click(cx.listener(move |this, _, _, cx| {
                    this.chip_menu = if this.chip_menu == Some(id) { None } else { Some(id) };
                    cx.notify();
                })))
            })
            .child(remove)
    }

    fn picture_chip(&self, chip: &Staged, picture: Picture, options: Options, cx: &mut Context<Self>) -> AnyElement {
        let shown = match picture {
            Picture::Ready(image) => img(ImageSource::Render(image)).size(px(THUMB)).into_any_element(),
            _ => div().size(px(THUMB)).bg(theme::raised()).into_any_element(),
        };
        let tags = tags(chip);
        div()
            .id(("chip", chip.id))
            .relative()
            .flex_none()
            .size(px(THUMB))
            .rounded(px(theme::R_CARD))
            .overflow_hidden()
            .border_1()
            .border_color(theme::hairline())
            .child(shown)
            .when(chip.spoiler, |chip| {
                chip.child(
                    div().absolute().top_0().left_0().size_full().bg(media_dim()).flex().items_center().justify_center().child(glyph("icons/eye-off.svg", 16., theme::on_accent())),
                )
            })
            .when(!tags.is_empty(), |chip| {
                chip.child(div().absolute().left(px(4.)).bottom(px(4.)).flex().gap(px(2.)).children(tags.into_iter().map(tag)))
            })
            .child(self.chip_controls(chip, options, cx))
            .children(self.chip_menu_layer(chip, options, cx))
            .into_any_element()
    }

    fn file_chip(&self, chip: &Staged, options: Options, cx: &mut Context<Self>) -> AnyElement {
        let refused = chip.refused.clone();
        let second = match (&refused, chip.converting) {
            (Some(why), _) => why.clone(),
            (None, true) => "Making a JPEG…".to_owned(),
            (None, false) if chip.bytes > 0 => format!("{} · {}", Kind::word(&chip.name), attach::size(chip.bytes)),
            (None, false) => Kind::word(&chip.name),
        };
        let icon = if refused.as_deref() == Some("Folders can't be sent") { "icons/circle-alert.svg" } else { file_icon(chip.kind) };
        let tile = div()
            .size(px(36.))
            .flex_none()
            .rounded(px(theme::R_CONTROL))
            .bg(if refused.is_some() { theme::danger_soft() } else { theme::selected() })
            .flex()
            .items_center()
            .justify_center()
            .child(glyph(icon, 16., if refused.is_some() { theme::danger() } else { theme::text_soft() }));
        let tags = tags(chip);
        div()
            .id(("chip", chip.id))
            .relative()
            .flex_none()
            .h(px(THUMB))
            .min_w(px(160.))
            .max_w(px(280.))
            .p(px(8.))
            // Room for remove, and for options where the chip has them.
            .pr(px(if options.any() { 56. } else { 32. }))
            .rounded(px(theme::R_CARD))
            .border_1()
            .map(|chip| match refused {
                Some(_) => chip.bg(theme::danger_soft()).border_color(theme::danger()),
                None => chip.bg(theme::field()).border_color(theme::border()),
            })
            .flex()
            .items_center()
            .gap(px(8.))
            .child(tile)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().type_ui().text_color(theme::text()).child(middle(&chip.name, 24)))
                    .child(div().truncate().type_small().text_color(if chip.refused.is_some() { theme::danger() } else { theme::text_soft() }).child(second)),
            )
            .when(!tags.is_empty(), |chip| chip.child(div().absolute().left(px(4.)).bottom(px(4.)).flex().gap(px(2.)).children(tags.into_iter().map(tag))))
            .child(self.chip_controls(chip, options, cx))
            .children(self.chip_menu_layer(chip, options, cx))
            .into_any_element()
    }

    /// The ⋯ menu: Mark as spoiler, Send as file, Send original, each where it applies, a
    /// check on what is set.
    fn chip_menu_layer(&self, chip: &Staged, options: Options, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.chip_menu != Some(chip.id) || !options.any() {
            return None;
        }
        let id = chip.id;
        // A check on what is set; the slot stays empty otherwise, so the labels line up.
        let check = |on: bool| on.then_some("icons/check.svg");
        let slot = |item: gpui::Stateful<Div>, on: bool| if on { item } else { item.pl(px(32.)) };
        let mut items = menu().w(px(220.));
        if options.spoiler {
            items = items.child(slot(menu_item(("opt-spoiler", id), check(chip.spoiler), "Mark as spoiler", None), chip.spoiler).on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_chip(id, |chip| chip.spoiler = !chip.spoiler, cx);
            })));
        }
        if options.as_file {
            items = items.child(slot(menu_item(("opt-file", id), check(chip.as_file), "Send as file", None), chip.as_file).on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_chip(id, |chip| chip.as_file = !chip.as_file, cx);
            })));
        }
        if options.original {
            items = items.child(slot(menu_item(("opt-original", id), check(chip.original), "Send original (HEIC)", None), chip.original).on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_chip(id, |chip| chip.original = !chip.original, cx);
            })));
        }
        let items = items.on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.chip_menu = None;
            cx.notify();
        }));
        let layer = deferred(anchored().anchor(Anchor::BottomLeft).offset(point(px(0.), px(-6.))).snap_to_window_with_margin(px(8.)).child(items)).with_priority(2);
        Some(div().absolute().top_0().left_0().child(layer).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::middle;

    #[test]
    fn a_long_name_keeps_its_start_and_its_extension() {
        assert_eq!(middle("trail.pdf", 24), "trail.pdf");
        let cut = middle("Trail map for the autumn foray (final).pdf", 24);
        assert!(cut.starts_with("Trail map") && cut.ends_with(").pdf") && cut.contains('…'), "{cut}");
        assert!(cut.chars().count() <= 24);
        assert_eq!(middle("ééééééééééééééééééééééééééééé.txt", 10).chars().count(), 10);
    }
}
