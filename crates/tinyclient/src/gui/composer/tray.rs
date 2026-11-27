//! The chips above the field: a picture's thumbnail or a file's tile, its name and size, why it
//! is refused, the tags of what is set on it, remove, and the options it has. Thumbnails are
//! decoded off the main thread at 128 px; the tray holds paths, never bytes.

use super::attach::{self, Kind, Options, Staged};
use super::picker::images::{Loaded, Picture};
use super::Composer;
use crate::gui::components::place::{placed, Layer, Place};
use crate::gui::components::{glyph, menu, menu_item, tip};
use crate::gui::theme::{self, Type};
use crate::gui::anim::{self, Play};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use gpui::{canvas, div, prelude::*, px, size, AnyElement, Bounds, Context, Div, Focusable, MouseButton, ObjectFit, Rgba, Window};

const THUMB: f32 = 72.;
const CONTROL: f32 = 18.;

/// Dim scrim over a picture being uploaded or marked spoiler, and darker chip under a tag.
fn media_dim() -> Rgba {
    theme::media_chip()
}

fn media_tag() -> Rgba {
    theme::media_dim()
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
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(|style| style.shadow(theme::focus_ring_inset()))
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

fn file_tag(words: &'static str) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .rounded(px(theme::R_CHIP))
        .bg(theme::selected())
        .border_1()
        .border_color(theme::hairline())
        .flex()
        .items_center()
        .text_size(theme::sized(11.))
        .line_height(theme::line(14.))
        .font_weight(theme::MEDIUM)
        .text_color(theme::text_soft())
        .child(words)
}

fn tag(words: &'static str) -> Div {
    div()
        .h(px(16.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .bg(media_tag())
        .flex()
        .items_center()
        .text_size(theme::sized(11.))
        .line_height(theme::line(14.))
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
    if !chip.alt.trim().is_empty() {
        tags.push("ALT");
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
        let mut row = div().id("tray").px(px(8.)).py(px(8.)).flex().gap(px(8.)).overflow_x_scroll();
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
        Some(
            div()
                .border_b_1()
                .border_color(theme::hairline())
                .child(crate::gui::components::scroll::scroll_content("tray-scroll-frame", row))
                .children(self.alt_row(window, cx)),
        )
    }

    fn chip_controls(&self, chip: &Staged, options: Options, cx: &mut Context<Self>) -> Div {
        let id = chip.id;
        let anchors = self.anchors.clone();
        let is_open = self.chip_menu == Some(id);
        let remove = corner(("chip-remove", id), "icons/x.svg")
            .aria_label(format!("Remove {}", chip.name))
            .on_click(cx.listener(move |this, _, _, cx| this.remove_chip(id, cx)));
        div()
            .absolute()
            .top(px(4.))
            .right(px(4.))
            .flex()
            .gap(px(3.))
            .when(options.any(), |controls| {
                controls.child(
                    corner(("chip-options", id), "icons/ellipsis.svg")
                        .relative()
                        .aria_label(format!("Options for {}", chip.name))
                        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                            if this.chip_menu == Some(id) {
                                this.chip_menu = None;
                            } else {
                                this.chip_menu = Some(id);
                                this.anchors.chip.set(Some(Bounds {
                                    origin: event.position(),
                                    size: size(px(CONTROL), px(CONTROL)),
                                }));
                            }
                            cx.notify();
                        }))
                        .when(is_open, |btn| {
                            btn.child(
                                canvas(move |bounds, _, _| anchors.chip.set(Some(bounds)), |_, _, _, _| {})
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                            )
                        }),
                )
            })
            .child(remove)
    }

    fn picture_chip(&self, chip: &Staged, picture: Picture, options: Options, cx: &mut Context<Self>) -> AnyElement {
        let shown = match picture {
            Picture::Ready(image) => div().size(px(THUMB)).child(anim::picture(image, ObjectFit::Cover, 0., Play::Inline)).into_any_element(),
            _ => div().size(px(THUMB)).bg(theme::raised()).into_any_element(),
        };
        let tags = tags(chip);
        div()
            .id(("chip", chip.id))
            .role(gpui::Role::Group)
            .aria_label(format!("Attachment: {}", chip.name))
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
            .into_any_element()
    }

    fn file_chip(&self, chip: &Staged, options: Options, cx: &mut Context<Self>) -> AnyElement {
        let refused = chip.refused.clone();
        let preview = super::rec::preview(&self.note, chip);
        let playback = preview.as_ref().and_then(|play| {
            self.shell.upgrade().and_then(|shell| shell.read(cx).playing_state.of(&play.account, &play.file).cloned())
        });
        let playing = playback.as_ref().is_some_and(|shown| matches!(shown.state, protocol::PlayState::Playing | protocol::PlayState::Loading));
        let second = if preview.is_some() {
            let duration = match self.note.as_ref().map(|note| &note.state) {
                Some(protocol::RecordState::Recorded { duration, .. }) => *duration,
                _ => 0,
            };
            super::rec::preview_caption(duration, playback.as_ref(), crate::gui::playing::now_ms(), self.note.as_ref().is_some_and(|note| note.video))
        } else {
            match (&refused, chip.converting) {
                (Some(why), _) => why.clone(),
                (None, true) => "Making a JPEG…".to_owned(),
                (None, false) if chip.bytes > 0 => format!("{} · {}", Kind::word(&chip.name), attach::size(chip.bytes)),
                (None, false) => Kind::word(&chip.name),
            }
        };
        let icon = if refused.as_deref() == Some("Folders can't be sent") { "icons/circle-alert.svg" } else { file_icon(chip.kind) };
        let tile = div()
            .id(("voice-preview", chip.id))
            .size(px(36.))
            .flex_none()
            .rounded(px(theme::R_CONTROL))
            .bg(if refused.is_some() { theme::danger_soft() } else { theme::selected() })
            .flex()
            .items_center()
            .justify_center()
            .map(|tile| match preview {
                Some(_) => {
                    let id = chip.id;
                    tile.cursor_pointer()
                        .tooltip(tip(
                            if playing {
                                "Pause preview"
                            } else if self.note.as_ref().is_some_and(|note| note.video) {
                                "Play video message"
                            } else {
                                "Play voice message"
                            },
                            None,
                        ))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| this.preview_pressed(id, cx)))
                        .child(crate::gui::conversation::player_glyph(playing, 16., theme::accent_text()))
                }
                None => tile.child(glyph(icon, 16., if refused.is_some() { theme::danger() } else { theme::text_soft() })),
            });
        let tags = tags(chip);
        div()
            .id(("chip", chip.id))
            .role(gpui::Role::Group)
            .aria_label(format!("Attachment: {}", chip.name))
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
                    .child(div().truncate().type_ui().text_color(theme::text()).child(if super::rec::holds_the_note(&self.note, chip) {
                        if self.note.as_ref().is_some_and(|note| note.video) { "Video message".to_owned() } else { "Voice message".to_owned() }
                    } else {
                        middle(&chip.name, 24)
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(div().min_w_0().truncate().type_small().text_color(if chip.refused.is_some() { theme::danger() } else { theme::text_soft() }).child(second))
                            .children(tags.into_iter().map(file_tag)),
                    ),
            )
            .child(self.chip_controls(chip, options, cx))
            .into_any_element()
    }

    fn preview_pressed(&mut self, id: u64, cx: &mut Context<Self>) {
        if !super::live() {
            return;
        }
        let Some(play) = self.tray.iter().find(|chip| chip.id == id).and_then(|chip| super::rec::preview(&self.note, chip)) else { return };
        self.shell_later(cx, move |shell, cx| {
            let playing = shell.playing_state.of(&play.account, &play.file)
                .is_some_and(|shown| matches!(shown.state, protocol::PlayState::Playing | protocol::PlayState::Loading));
            if playing {
                shell.play_pause(cx);
            } else {
                shell.play(play, cx);
            }
        });
    }

    /// The ⋯ menu: Mark as spoiler, Send as file, Send original, each where it applies, a
    /// check on what is set. Rendered at top level via `placed(..., Layer::Menu)` so it is never
    /// clipped by the card's `overflow_hidden` or painted beneath window buttons.
    pub(super) fn chip_menu_layer(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.chip_menu?;
        let chip = self.tray.iter().find(|chip| chip.id == id)?;
        let (can, network) = self.target.as_ref().map(|target| (target.can, target.key.network))?;
        let options = attach::options(chip, &can, network);
        if !options.any() {
            return None;
        }
        let anchor = self.anchors.chip.get().or_else(|| self.anchors.dock.get())?;
        // A check on what is set; the slot stays empty otherwise, so the labels line up.
        let check = |on: bool| on.then_some("icons/check.svg");
        let slot = |item: gpui::Stateful<Div>, on: bool| if on { item } else { item.pl(px(32.)) };
        let max_w = (f32::from(window.viewport_size().width) - 16.).max(160.);
        let mut items = menu().w(px(220.0_f32.min(max_w)));
        if options.spoiler {
            items = items.child(slot(menu_item(("opt-spoiler", id), check(chip.spoiler), "Mark as spoiler", None), chip.spoiler).on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_chip(id, |chip| chip.spoiler = !chip.spoiler, cx);
            })));
        }
        if options.alt_text {
            let described = !chip.alt.trim().is_empty();
            items = items.child(slot(menu_item(("opt-alt", id), check(described), "Add description", None), described).on_click(cx.listener(move |this, _, window, cx| {
                this.begin_alt(id, window, cx);
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
        let items = items
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.chip_menu = None;
                cx.notify();
            }));
        Some(placed("composer-chip-menu", Place::Above(anchor), Layer::Menu, window, items))
    }

    /// The description field under the chips, while one picture is being described.
    fn alt_row(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let (id, field) = self.alt_for.clone()?;
        if !self.tray.iter().any(|chip| chip.id == id) {
            return None;
        }
        let focused = field.focus_handle(cx).is_focused(window);
        Some(
            div()
                .px(px(8.))
                .pb(px(8.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(crate::gui::components::field(focused).child(div().flex_1().min_w_0().child(field))),
        )
    }

    /// Opens the description field for one chip. Return keeps it, Escape puts back what it was.
    fn begin_alt(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.chip_menu = None;
        let previous = self.tray.iter().find(|chip| chip.id == id).map(|chip| chip.alt.clone()).unwrap_or_default();
        let field = cx.new(|cx| TextInput::new("Describe this picture", cx));
        field.update(cx, |field, cx| field.set_text(&previous, cx));
        cx.subscribe(&field, |this, _, event: &InputEvent, cx| {
            let finish = matches!(event, InputEvent::Submit);
            if finish || matches!(event, InputEvent::Changed) {
                this.keep_alt(finish, cx);
            }
            if finish {
                this.alt_for = None;
                this.focus_next = true;
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&field, move |this, _, event: &InputKey, cx| {
            if *event == InputKey::Escape {
                if let Some(chip) = this.tray.iter_mut().find(|chip| chip.id == id) {
                    chip.alt = previous.clone();
                }
                this.alt_for = None;
                this.focus_next = true;
                cx.notify();
            }
        })
        .detach();
        window.focus(&field.focus_handle(cx), cx);
        self.alt_for = Some((id, field));
        cx.notify();
    }

    /// Copies the field onto the chip. `finish` trims it; while typing only the length is cut,
    /// so a space between words stays until Return.
    fn keep_alt(&mut self, finish: bool, cx: &mut Context<Self>) {
        let Some((id, field)) = self.alt_for.clone() else { return };
        let typed = field.read(cx).text().to_owned();
        let limited: String = typed.chars().take(protocol::ALT_LIMIT).collect();
        if limited != typed {
            field.update(cx, |field, cx| field.set_text(&limited, cx));
        }
        if let Some(chip) = self.tray.iter_mut().find(|chip| chip.id == id) {
            chip.alt = if finish { protocol::clip_alt(&limited) } else { limited };
        }
        cx.notify();
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
