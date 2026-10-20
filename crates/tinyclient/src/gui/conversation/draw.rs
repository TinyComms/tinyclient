//! How each row is drawn. No bubbles for anyone: avatar, name, time, body, the same on every
//! network. Every box that holds a picture is sized from what the network said about it, so a
//! row is its final height before any file arrives.

use super::emoji;
use super::icons::{icon, Icon};
use super::images::Picture;
use super::media::{self, Fit, Shape as MediaShape};
use super::rich::{self, Block, Inline, Jumbo};
use super::rows::Item;
use super::text::{Look, Rich, TextEvent};
use super::tokens::{self, Metrics, MEASURE};
use super::when;
use super::{Pane, PaneEvent, Selection, Then};
use crate::gui::anim::{self, Play};
use crate::gui::components::{glyph, tip, Avatar, Shape};
use crate::gui::theme::{self, Type};
use crate::gui::time;
use crate::view::{age, now};
use gpui::{
    canvas, div, prelude::*, px, Animation, AnimationExt, AnyElement, ClipboardItem, Context, Div, ElementId,
    Hsla, MouseButton, ObjectFit, SharedString, Stateful, Window,
};
use protocol::{Attachment, Card, Forwarded, Key, MediaFile, MentionKind, Message, MessageKind, Preview, SendState};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

/// How long the tint on a jump's target takes to fade.
pub const FLASH: Duration = Duration::from_millis(1200);
/// Lines of a code block shown before "Show more".
const CODE_LINES: usize = 20;
/// Link cards drawn per message; further links stay text.
const CARDS: usize = 3;
/// Reaction chips drawn before the rest are counted.
const CHIPS: usize = 12;

/// Where files are linked under their own names to be opened: `open/` in the support
/// directory, emptied at each start. The cache names files by a hash with no extension, which
/// no player, Quick Look or default app can tell the type of.
fn opened_dir() -> std::path::PathBuf {
    protocol::support_dir().join("open")
}

/// Empties [`opened_dir`], once a run. A player that still has a file open keeps it: the
/// link goes, the bytes stay until it closes them.
fn clear_opened() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| drop(std::fs::remove_dir_all(opened_dir())));
}

/// The cached file at `path` under `name`: a hard link in [`opened_dir`], so no bytes are
/// copied; a copy where a link cannot be made; the file itself if neither works.
fn named(path: &str, name: &str) -> std::path::PathBuf {
    clear_opened();
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    let dir = opened_dir().join(format!("{hash:016x}"));
    let target = dir.join(name);
    if target.exists() {
        return target;
    }
    let made = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::hard_link(path, &target).or_else(|_| std::fs::copy(path, &target).map(drop)));
    match made {
        Ok(()) => target,
        Err(err) => {
            eprintln!("tinyclient: could not name {name} for opening: {err}");
            std::path::PathBuf::from(path)
        }
    }
}

/// Opens a cached file under its own name: in Quick Look, or in the system's app for it (a
/// video in the system's player). Both are processes of their own; a thread waits for them so
/// nothing of ours blocks, and makes the link off the main thread.
pub fn open_file(path: &str, name: &str, then: Then) {
    let (path, name) = (path.to_owned(), name.to_owned());
    std::thread::spawn(move || {
        let path = named(&path, &name);
        let mut command;
        #[cfg(target_os = "macos")]
        {
            command = match then {
                Then::QuickLook => Command::new("/usr/bin/qlmanage"),
                Then::Open => Command::new("/usr/bin/open"),
            };
            if then == Then::QuickLook {
                command.arg("-p");
            }
            command.arg(&path);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = then;
            command = Command::new("xdg-open");
            command.arg(&path);
        }
        #[cfg(windows)]
        {
            let _ = then;
            command = Command::new("cmd");
            command.arg("/C").arg("start").arg("").arg(&path);
            protocol::launch::no_console(&mut command);
        }
        let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
    });
}

fn name_id(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("{prefix}-{id}")))
}

/// `9:07`, for the gutter of a continuation row, which is too narrow for the half of the day.
fn short_clock(unix: i64) -> String {
    let clock = time::clock(unix);
    clock.split(' ').next().unwrap_or_default().to_owned()
}

fn day_line(at: i64, metrics: Metrics) -> Div {
    let rule = || div().flex_1().h(px(1.)).bg(theme::hairline());
    div()
        .h(px(36.))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(rule())
        .child(div().type_micro().text_color(theme::text_faint()).child(when::day_label(now(), at)))
        .child(rule())
}

/// The line above the first unread message: accent, ending in a filled tab.
fn new_line(metrics: Metrics) -> Div {
    div()
        .h(px(24.))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().flex_1().h(px(1.)).bg(theme::accent()))
        .child(
            div()
                .flex_none()
                .text_size(px(11.))
                .line_height(px(14.))
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("New"),
        )
}

fn replies_line(count: u32, metrics: Metrics) -> Div {
    let words = if count == 1 { "1 reply".to_owned() } else { format!("{count} replies") };
    div()
        .h(px(32.))
        .px(px(metrics.inset))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().type_micro().text_color(theme::text_faint()).child(words))
        .child(div().flex_1().h(px(1.)).bg(theme::hairline()))
}

fn system_line(message: &Message) -> Div {
    div()
        .min_h(px(28.))
        .px(px(20.))
        .py(px(5.))
        .flex()
        .justify_center()
        .type_small()
        .text_color(theme::text_faint())
        .child(div().max_w(px(MEASURE)).child(message.text.clone()))
}

/// A glyph and a line of words in the middle of a media box.
fn in_box(which: Icon, words: Option<&'static str>) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .type_small()
        .text_color(theme::text_faint())
        .child(icon(which, 20., theme::text_faint()))
        .children(words)
}

/// The label over a still that says what a click will do.
pub(super) fn badge(words: String) -> Div {
    div()
        .absolute()
        .bottom(px(6.))
        .h(px(20.))
        .px(px(6.))
        .rounded(px(5.))
        .bg(tokens::badge())
        .flex()
        .items_center()
        .type_micro()
        .font_weight(theme::SEMIBOLD)
        .text_color(gpui::white())
        .child(words)
}

pub(super) fn play_disc() -> Div {
    div().absolute().inset_0().flex().items_center().justify_center().child(
        div()
            .size(px(44.))
            .rounded_full()
            .bg(tokens::badge())
            .flex()
            .items_center()
            .justify_center()
            .child(icon(Icon::Play, 18., gpui::white())),
    )
}

/// A small square button inside a row or a bar.
fn tool(id: impl Into<ElementId>, which: Icon, size: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(size))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()))
        .active(|style| style.bg(theme::pressed()))
        .child(icon(which, 16., theme::text_soft()))
}

impl Pane {
    pub(super) fn row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let metrics = self.metrics;
        let body = match &item {
            Item::Start => self.start_row(window, cx).into_any_element(),
            Item::Older => self.older_row(cx).into_any_element(),
            Item::Root(message) => self.message_row(message, true, true, window, cx),
            Item::Replies(count) => replies_line(*count, metrics).into_any_element(),
            Item::Day(at) => day_line(*at, metrics).into_any_element(),
            Item::New => new_line(metrics).into_any_element(),
            Item::Message { message, head } => self.message_row(message, *head, false, window, cx),
            Item::System(message) => system_line(message).into_any_element(),
        };
        let seen = self.seen.clone();
        let mark = canvas(move |bounds, _, _| seen.borrow_mut().push((ix, bounds)), |_, (), _, _| {});
        div()
            .relative()
            .w_full()
            .child(body)
            .child(mark.absolute().top_0().left_0().size_full())
            .into_any_element()
    }

    /// The widest a picture may be: 400, or what the pane leaves beside the gutter.
    fn media_width(&self) -> f32 {
        // The list is laying rows out when this runs, so its width is the one noted after the
        // last draw.
        let pane = self.width;
        let room = pane - self.metrics.gutter() - self.metrics.inset;
        if pane <= 0. {
            self.metrics.media
        } else {
            self.metrics.media.min(room).max(120.).floor()
        }
    }

    fn start_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(open) = &self.open else {
            return div();
        };
        const SIZE: f32 = 80.;
        let direct = open.kind.is_direct();
        let words = if direct {
            format!("This is the start of your conversation with {}.", open.title)
        } else {
            format!("This is the start of #{}.", open.title.trim_start_matches('#'))
        };
        let plate = if direct { Avatar::new(&open.key, &open.title, SIZE) } else { Avatar::channel("icons/hash.svg", SIZE) };
        let shape = if direct { Shape::Person } else { Shape::Place };
        let (title, file) = (open.title.clone(), open.picture.clone());
        let picture = self.face(plate.shape(shape), file.as_ref(), SIZE, shape, window, cx);
        div()
            .px(px(self.metrics.inset))
            .pt(px(32.))
            .pb(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(picture)
            .child(div().type_headline().child(title))
            .child(div().type_ui().text_color(theme::text_soft()).child(words))
    }

    /// An avatar with its picture drawn from this pane's own decodes, asked for at the size it
    /// is shown: the plate holds the box, and the picture covers it once it is here.
    fn face(&mut self, plate: Avatar, file: Option<&MediaFile>, size: f32, shape: Shape, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let edge = media::square_size(size, window.scale_factor());
        let radius = match shape {
            Shape::Person => size / 2.,
            Shape::Place => (size * 0.25).round(),
        };
        let picture = file.and_then(|file| match self.still(file, edge, edge, Some(1.), cx) {
            Picture::Ready(image) => {
                Some(div().absolute().top_0().left_0().size(px(size)).child(anim::picture(image, ObjectFit::Cover, radius, Play::Hover)))
            }
            _ => None,
        });
        plate.build().children(picture)
    }

    fn older_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = div()
            .id("older")
            .h(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .type_small()
            .text_color(theme::text_faint());
        match &self.older_failed {
            None if self.window.messages.is_empty() => row.child("Loading messages…"),
            None => row.child("Loading earlier messages…"),
            Some(_) => row.child(if self.window.messages.is_empty() { "Couldn't load messages ·" } else { "Couldn't load earlier messages ·" }).child(
                div()
                    .id("older-retry")
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .child("Retry")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.older_failed = None;
                        cx.notify();
                    })),
            ),
        }
    }

    /// Floats 16 from the right, 12 above the composer: "12 new" while unread is below, else
    /// "Jump to latest". A click, or Escape with nothing else open, goes to the newest.
    pub(super) fn jump_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let words = if self.unseen > 0 { format!("{} new", self.unseen) } else { "Jump to latest".to_owned() };
        div()
            .id("jump")
            .absolute()
            .bottom(px(12.))
            .right(px(16.))
            .h(px(28.))
            .pl(px(8.))
            .pr(px(10.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::border())
            .shadow(theme::shadow_popover())
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::over(theme::hover(), theme::overlay())))
            .active(|style| style.bg(theme::over(theme::pressed(), theme::overlay())))
            .child(glyph("icons/arrow-down.svg", 16., theme::text_soft()))
            .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(words))
            .on_click(cx.listener(|this, _, _, cx| this.jump(cx)))
            .into_any_element()
    }

    // The message.

    fn message_row(
        &mut self,
        message: &Rc<Message>,
        head: bool,
        root: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let metrics = self.metrics;
        let failed = matches!(message.state, SendState::Failed { .. });
        let pending = message.state == SendState::Pending;
        let mentioned = message.mentions_me && !failed;
        let bar = match (failed, message.mentions_me) {
            (true, _) => Some(theme::danger()),
            (false, true) => Some(theme::accent()),
            (false, false) => None,
        };
        let flashing = self.flash.as_deref() == Some(message.id.as_str());
        // The message in the composer for editing is marked, so it is plain which one is changing.
        let editing = self.shell.upgrade().and_then(|shell| shell.read(cx).composer_editing(cx));
        let editing = !self.is_thread() && editing.as_deref() == Some(message.id.as_str());
        let bar = if editing { Some(theme::accent()) } else { bar };
        let owns = self.owns_overlay(&message.id);

        let gutter = if head {
            self.avatar(message, window, cx).into_any_element()
        } else {
            div()
                .w(px(metrics.avatar))
                .h(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .type_micro()
                .text_color(gpui::transparent_black())
                .group_hover("row", |style| style.text_color(theme::text_faint()))
                .child(short_clock(message.sent))
                .into_any_element()
        };

        let mut sent = div().flex().flex_col();
        if message.deleted {
            // What it said is gone; the row stays for the thread that hangs off it.
            sent = sent.child(div().type_body().italic().text_color(theme::text_faint()).child("Message deleted"));
        } else {
            let said = match &message.kind {
                MessageKind::Unsupported { link } => vec![self.unsupported(message, link, cx).into_any_element()],
                _ => self.content(message, window, cx),
            };
            let attached = self.attachments(message, window, cx);
            sent = match &message.forwarded {
                // What was forwarded is the message itself, set behind a bar under where it came from.
                Some(from) => sent.child(forwarded_line(from)).child(
                    div()
                        .flex()
                        .child(div().w(px(2.)).flex_none().rounded(px(1.)).bg(tokens::border_strong()))
                        .child(div().pl(px(12.)).min_w_0().flex_1().flex().flex_col().children(said).children(attached)),
                ),
                None => sent.children(said).children(attached),
            };
            sent = sent.children(message.card.as_deref().map(|card| self.app_card(card))).children(self.cards(message, window, cx));
        }
        if pending {
            sent = sent.opacity(0.55);
        }
        let column = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .children(head.then(|| self.header(message)))
            .child(sent)
            .children(self.reactions(message, window, cx))
            .children(self.thread_line(message, window, cx))
            .children(self.sending(message, cx))
            .children(self.unsent(message, cx));

        let menu = message.clone();
        let inner = div()
            .id(name_id("m", &message.id))
            .group("row")
            .relative()
            .px(px(metrics.inset))
            .when(head, |row| row.pt(px(3.)).pb(px(3.)))
            .when(!head, |row| row.py(px(1.)))
            .when(editing, |row| row.bg(theme::accent_soft()))
            .when(mentioned && !editing, |row| row.bg(theme::accent_wash()))
            // The row that owns an open menu, picker or dialog keeps `selected` (polish/01 §2).
            .when(owns && !editing, |row| row.bg(theme::selected()))
            .when(!mentioned && !editing && !owns, |row| row.hover(|style| style.bg(tokens::row_hover())))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    let can = this.can(&menu);
                    if can.act || can.unsent {
                        this.open_menu(menu.clone(), event.position, cx);
                    }
                }),
            )
            .children(bar.map(|color| div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(color)))
            .children(flashing.then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .bg(theme::accent_soft())
                    .with_animation(name_id("flash", &message.id), Animation::new(FLASH), |tint, done| tint.opacity(1. - done))
            }))
            .children(message.reply_to.as_ref().map(|reply| self.reply_line(reply, window, cx)))
            .child(div().flex().gap(px(metrics.gap)).child(gutter).child(column))
            // Last, so it is over the row; it rises past the row's top edge, which nothing clips.
            .children(self.toolbar(message, cx));
        // The gap above a group is outside the row, so it does not light up on hover.
        div().w_full().when(head && !root, |row| row.pt(px(8.))).child(inner).into_any_element()
    }

    fn person(&self, id: &str) -> Option<Key> {
        let open = self.open.as_ref()?;
        (!id.is_empty()).then(|| Key { id: id.to_owned(), ..open.key.clone() })
    }

    /// Someone in this conversation, from what its messages say of them, else from the people
    /// hub: their name and their picture.
    fn who(&self, person: &Key, cx: &mut Context<Self>) -> (String, Option<MediaFile>) {
        if let Some(message) = said_by(self.root(), &self.window.messages, &person.id) {
            return (message.sender.name.clone(), message.sender.avatar.clone());
        }
        let known = self.shell.upgrade().and_then(|shell| shell.read(cx).people_state.hub().person(person));
        known.map(|person| (person.name, person.avatar)).unwrap_or_default()
    }

    fn root(&self) -> Option<&Rc<Message>> {
        self.open.as_ref().and_then(|open| open.root.as_ref()).map(|(root, _)| root)
    }

    /// My own picture on this account.
    fn own_picture(&self, cx: &mut Context<Self>) -> Option<MediaFile> {
        let key = self.key()?;
        self.shell.upgrade().and_then(|shell| shell.read(cx).own_picture(key))
    }

    fn avatar(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let sender = &message.sender;
        let size = self.metrics.avatar;
        let me = self.open.as_ref().map(|open| open.me.clone()).unwrap_or_default();
        let id = if message.outgoing && sender.id.is_empty() { me.as_str() } else { sender.id.as_str() };
        let plate = match self.person(id) {
            Some(key) => Avatar::new(&key, &sender.name, size),
            None => Avatar::named(&sender.name, size),
        };
        let file = match (&sender.avatar, message.outgoing) {
            (_, true) => own_face(message, self.root(), &self.window.messages, &me, || self.own_picture(cx)),
            (Some(file), false) => Some(file.clone()),
            // A tombstone names its sender and nothing else; their other messages have the picture.
            (None, false) => self.person(id).and_then(|key| self.who(&key, cx).1),
        };
        // An app never reads as a person: its picture is squared.
        let shape = if sender.bot { Shape::Place } else { Shape::Person };
        let face = self.face(plate.shape(shape), file.as_ref(), size, shape, window, cx);
        div().pt(px(2.)).flex_none().when(message.deleted, |avatar| avatar.opacity(0.4)).child(face)
    }

    fn header(&self, message: &Message) -> Div {
        let name = if message.outgoing { "You".to_owned() } else { message.sender.name.clone() };
        let tag = |words: &'static str| {
            div()
                .flex_none()
                .h(px(16.))
                .px(px(4.))
                .rounded(px(theme::R_CHIP))
                .bg(theme::raised())
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::text_soft())
                .child(words)
        };
        div()
            .h(px(20.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().min_w_0().truncate().type_body().font_weight(theme::SEMIBOLD).child(name))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .line_height(px(20.))
                            .font_weight(theme::MEDIUM)
                            .text_color(theme::text_faint())
                            .child(time::clock(message.sent)),
                    )
                    .children(silent_mark(message)),
            )
            .children(message.sender.bot.then(|| tag("APP")))
            .children(message.pinned.then(|| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(icon(Icon::Pin, 12., theme::text_faint()))
                    .child("Pinned")
            }))
    }

    // Words.

    fn content(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let edited = || div().h(px(16.)).type_micro().text_color(theme::text_faint()).child("(edited)").into_any_element();
        if message.text.trim().is_empty() {
            return message.edited.then(edited).into_iter().collect();
        }
        if let Some(emoji) = rich::jumbo(&message.text, &message.spans) {
            return vec![self.jumbo(&emoji, window, cx)];
        }
        let mut blocks = rich::blocks(&message.text, &message.spans);
        let mut edited_alone = message.edited;
        if let (true, Some(Block::Text(last))) = (message.edited, blocks.last_mut()) {
            last.mark_edited();
            edited_alone = false;
        }
        let mut out = Vec::with_capacity(blocks.len() + 1);
        for (ix, block) in blocks.iter().enumerate() {
            let element = match block {
                Block::Text(inline) => div()
                    .max_w(px(MEASURE))
                    .type_body()
                    .child(self.rich(message, ix, inline, theme::text().into(), window, cx))
                    .into_any_element(),
                Block::Quote(inline) => div()
                    .max_w(px(MEASURE))
                    .mt(px(3.))
                    .flex()
                    .child(div().w(px(3.)).flex_none().rounded(px(2.)).bg(tokens::border_strong()))
                    .child(
                        div()
                            .pl(px(12.))
                            .min_w_0()
                            .type_body()
                            .child(self.rich(message, ix, inline, theme::text_soft().into(), window, cx)),
                    )
                    .into_any_element(),
                Block::Item { ordered, depth, number, text } => {
                    let marker = if *ordered { format!("{number}.") } else { "•".to_owned() };
                    div()
                        .max_w(px(MEASURE))
                        .pl(px(22. * f32::from(*depth)))
                        .flex()
                        .type_body()
                        .child(div().w(px(22.)).flex_none().text_color(theme::text_faint()).child(marker))
                        .child(div().min_w_0().flex_1().child(self.rich(message, ix, text, theme::text().into(), window, cx)))
                        .into_any_element()
                }
                Block::Code { language, text } => self.code_block(message, ix, language, text, cx),
            };
            out.push(element);
        }
        if edited_alone {
            out.push(edited());
        }
        out
    }

    /// The decoded picture of a file on disk, `edge` pixels on its longer side. Asking queues
    /// the decode; the row is drawn again when it is done.
    pub(super) fn picture(&mut self, path: &str, edge: u32, crop: Option<f32>, cx: &mut Context<Self>) -> Picture {
        self.images.get(path, edge, crop, cx, Self::decoded)
    }

    pub(super) fn decoded(&mut self, loaded: super::images::Loaded, cx: &mut Context<Self>) {
        if self.images.store(loaded) {
            cx.notify();
        }
        self.images.pump(cx, Self::decoded);
    }

    fn rich(
        &mut self,
        message: &Message,
        block: usize,
        inline: &Inline,
        color: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Rich {
        let hovered = self.hovered.as_ref().filter(|(id, at, _)| *id == message.id && *at == block).map(|(_, _, run)| *run);
        let selected = self.selection.as_ref().filter(|s| s.message == message.id);
        let selection = selected.and_then(|s| s.within(block));
        let dragging = self.dragging && selected.is_some();
        let revealed: Vec<u32> = self.revealed.iter().filter(|(id, _)| *id == message.id).map(|(_, span)| *span).collect();
        let me = self.open.as_ref().map(|open| open.me.clone()).unwrap_or_default();
        let edge = media::square_size(18., window.scale_factor());
        let this = cx.weak_entity();
        let id = message.id.clone();
        let mut emoji = |file: &MediaFile| match self.still(file, edge, edge, None, cx) {
            Picture::Ready(image) => Some(image),
            _ => None,
        };
        let look = Look {
            color,
            hovered,
            revealed: &|span| revealed.contains(&span),
            me: &me,
            mentions_me: message.mentions_me,
            emoji: &mut emoji,
            selection,
            dragging,
        };
        Rich::new(("text", block), inline, look, move |event, window, cx| {
            let _ = this.update(cx, |this, cx| this.text_event(&id, block, event, window, cx));
        })
    }

    fn text_event(&mut self, id: &str, block: usize, event: TextEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TextEvent::Hover(Some(run)) => {
                self.hovered = Some((id.to_owned(), block, run));
                cx.notify();
            }
            TextEvent::Hover(None) => {
                // Only the block that had the pointer may say it has gone.
                if self.hovered.as_ref().is_some_and(|(at, in_block, _)| at == id && *in_block == block) {
                    self.hovered = None;
                    cx.notify();
                }
            }
            TextEvent::Down(at) => {
                // Shift extends what is selected in this message; a plain press starts over.
                let extend = window.modifiers().shift;
                match self.selection.as_mut().filter(|selection| extend && selection.message == id) {
                    Some(selection) => selection.head = Some((block, at)),
                    None => self.selection = Some(Selection { message: id.to_owned(), anchor: (block, at), head: None }),
                }
                self.dragging = true;
                self.on_text = true;
                cx.notify();
            }
            TextEvent::Drag(at) => {
                // Only the message the drag began in: its blocks speak, the others stay quiet.
                let Some(selection) = self.selection.as_mut().filter(|selection| self.dragging && selection.message == id) else {
                    return;
                };
                if selection.head != Some((block, at)) {
                    selection.head = Some((block, at));
                    cx.notify();
                }
            }
            TextEvent::Click(run) => {
                let Some(message) = self.find(id) else {
                    return;
                };
                let blocks = rich::blocks(&message.text, &message.spans);
                let inline = match blocks.get(block) {
                    Some(Block::Text(inline) | Block::Quote(inline) | Block::Item { text: inline, .. }) => inline,
                    _ => return,
                };
                let Some(run) = inline.runs.get(run) else {
                    return;
                };
                if let Some(spoiler) = run.spoiler {
                    if self.revealed.insert((id.to_owned(), spoiler)) {
                        cx.notify();
                        return;
                    }
                }
                if let Some(url) = &run.link {
                    cx.open_url(url);
                } else if let Some((MentionKind::User, person)) = &run.mention {
                    let (Some(person), at) = (self.person(person), window.mouse_position()) else {
                        return;
                    };
                    let _ = self.shell.update(cx, |shell, cx| shell.open_profile(person, at, cx));
                }
                // LATER: a click on a channel mention opens the channel; it needs the channel's
                // row, which the sidebar holds.
            }
        }
    }

    fn jumbo(&mut self, emoji: &[Jumbo], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let edge = media::square_size(40., window.scale_factor());
        let mut line = div().h(px(48.)).flex().items_center().gap(px(4.));
        for one in emoji {
            line = match one {
                Jumbo::Unicode(glyphs) => line.child(div().text_size(px(40.)).line_height(px(48.)).child(glyphs.clone())),
                Jumbo::Custom { file, name } => {
                    match self.still(file, edge, edge, None, cx) {
                        Picture::Ready(image) => line.child(div().size(px(40.)).flex_none().child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline))),
                        _ => line.child(div().type_body().text_color(theme::text_soft()).child(name.clone())),
                    }
                }
            };
        }
        line.into_any_element()
    }

    fn code_block(&mut self, message: &Message, block: usize, language: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let key = (message.id.clone(), block);
        let lines = text.lines().count();
        let folded = lines > CODE_LINES && !self.expanded.contains(&key);
        let shown: String = if folded {
            text.lines().take(CODE_LINES).collect::<Vec<_>>().join("\n")
        } else {
            text.to_owned()
        };
        let copied = self.copied.as_ref() == Some(&key);
        let (copy, text) = (key.clone(), text.to_owned());
        let mut body = div().id(("code", block)).overflow_x_scroll().px(px(12.)).pt(px(2.)).pb(px(9.));
        // The wheel over a code block still scrolls the conversation; only sideways scrolls the code.
        body.style().restrict_scroll_to_axis = Some(true);
        div()
            .mt(px(5.))
            .mb(px(3.))
            .max_w(px(MEASURE))
            .rounded(px(theme::R_CARD))
            .bg(tokens::code_bg())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .child(
                div()
                    .h(px(28.))
                    .pl(px(12.))
                    .pr(px(2.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().type_micro().text_color(theme::text_faint()).child(language.to_owned()))
                    .child(
                        tool(("copy", block), if copied { Icon::Check } else { Icon::Copy }, 24.)
                            .tooltip(tip("Copy code", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                this.copied = Some(copy.clone());
                                cx.notify();
                                let copy = copy.clone();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.copied.as_ref() == Some(&copy) {
                                            this.copied = None;
                                            cx.notify();
                                        }
                                    });
                                })
                                .detach();
                            })),
                    ),
            )
            .child(body.child(div().type_code().whitespace_nowrap().child(shown)))
            .children(folded.then(|| {
                div()
                    .id(("more", block))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .type_small()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .child(format!("Show {} more lines", lines - CODE_LINES))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded.insert(key.clone());
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    fn unsupported(&self, message: &Message, link: &str, cx: &mut Context<Self>) -> Div {
        let network = self.open.as_ref().map_or("the app", |open| open.key.network.label());
        let which = if message.text.starts_with("Poll") { Icon::Poll } else { Icon::File };
        let link = link.to_owned();
        let line = div()
            .h(px(36.))
            .max_w_full()
            .px(px(12.))
            .rounded(px(theme::R_CARD))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(8.))
            .type_ui()
            .child(icon(which, 16., theme::text_soft()))
            .child(div().min_w_0().truncate().text_color(theme::text_soft()).child(message.text.clone()))
            .children((!link.is_empty()).then(|| {
                div()
                    .id("open-in")
                    .flex_none()
                    .text_color(theme::accent_text())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(format!("Open in {network}"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&link)))
            }));
        // A row of its own, so the box is as wide as its words and no wider.
        div().mt(px(2.)).flex().child(line)
    }

    // Pictures and files.

    fn attachments(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if message.attachments.is_empty() {
            return Vec::new();
        }
        let width = self.media_width();
        let shapes: Vec<MediaShape> = message.attachments.iter().map(media::shape).collect();
        let visual: Vec<usize> = (0..shapes.len()).filter(|ix| shapes[*ix].is_visual()).collect();
        let mut out = Vec::new();
        match visual.as_slice() {
            [] => {}
            [only] => {
                let attachment = &message.attachments[*only];
                let (wide, tall) = natural(attachment);
                let single = media::single(wide, tall, width);
                out.push(self.tile(message, *only, single.width, single.height, single.fit, None, window, cx));
            }
            many => {
                let (tiles, height) = media::grid(many.len(), width);
                let mut grid = div().relative().w(px(width)).h(px(height));
                for (slot, tile) in tiles.iter().enumerate() {
                    let more = (slot == 3 && many.len() > 4).then(|| many.len() - 4);
                    let picture = self.tile(message, many[slot], tile.width, tile.height, Fit::Cover, more, window, cx);
                    grid = grid.child(div().absolute().left(px(tile.x)).top(px(tile.y)).child(picture));
                }
                out.push(grid.into_any_element());
            }
        }
        for (ix, shape) in shapes.iter().enumerate() {
            match shape {
                MediaShape::Sticker => out.push(self.sticker(message, ix, window, cx)),
                MediaShape::Voice => out.push(self.player_row(message, ix, true, cx)),
                MediaShape::Audio => out.push(self.player_row(message, ix, false, cx)),
                MediaShape::File => out.push(self.file_row(message, ix, cx)),
                MediaShape::Image | MediaShape::Gif | MediaShape::Video => {}
            }
        }
        // 5 above the first, 4 between.
        out.into_iter()
            .enumerate()
            .map(|(ix, element)| div().pt(px(if ix == 0 { 5. } else { 4. })).flex().child(element).into_any_element())
            .collect()
    }

    /// Settings › Privacy › Load images (P3) off: a picture not on disk waits for a click, so
    /// nothing is fetched until then.
    fn held_back(&self, file: &MediaFile) -> bool {
        !crate::gui::app::prefs::choices().images && file.path.is_none() && !self.known.contains_key(&file.id) && !self.shown.contains(&file.id)
    }

    /// What stands in a held-back picture's box: what it is, its size, and the click that
    /// loads it.
    fn load_box(&self, holder: Stateful<Div>, file: &MediaFile, what: &str, bytes: u64, words: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = file.id.clone();
        let line = match (words, bytes) {
            (false, _) => "Load".to_owned(),
            (true, 0) => format!("{what} · Click to load"),
            (true, bytes) => format!("{what} · {} · Click to load", media::size_label(bytes)),
        };
        holder
            .child(div().absolute().inset_0().flex().items_center().justify_center().type_small().text_color(theme::text_soft()).child(line))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.shown.insert(id.clone());
                cx.notify();
            }))
    }

    /// The still of a file, or what stands in its box until it is on disk. `size` is the
    /// longest side the whole picture is shown at, which is what chatd is asked for; `edge` is
    /// what it is decoded to.
    pub(super) fn still(&mut self, file: &MediaFile, size: u32, edge: u32, crop: Option<f32>, cx: &mut Context<Self>) -> Picture {
        self.want(file, size);
        let path = file.path.clone().or_else(|| self.known.get(&file.id).cloned());
        match path {
            // A smaller copy on disk is drawn while a larger one is fetched, and if that fails.
            Some(path) => self.picture(&path, edge, crop, cx),
            None if file.failed.is_some() => Picture::Failed,
            None => Picture::Loading,
        }
    }

    /// What a picture's box shows when its file could not be fetched: the reason on hover, and
    /// a Retry that asks once per click.
    fn failed_box(&self, holder: Stateful<Div>, file: &MediaFile, words: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let reason = file.failed.clone().unwrap_or_else(|| "The file could not be read.".to_owned());
        let retry = file.clone();
        holder
            .child(in_box(Icon::ImageOff, words.then_some("Couldn't load image · Retry")))
            .tooltip(tip(format!("{reason} Click to try again."), None))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.retry_file(&retry, cx);
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn tile(
        &mut self,
        message: &Rc<Message>,
        index: usize,
        width: f32,
        height: f32,
        fit: Fit,
        more: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let attachment = &message.attachments[index];
        let shape = media::shape(attachment);
        let still = attachment.thumb.as_ref().unwrap_or(&attachment.file).clone();
        let edge_line = || div().absolute().inset_0().rounded(px(theme::R_CARD)).border_1().border_color(theme::hairline());
        let tile = div()
            .id(("tile", index))
            .relative()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .rounded(px(theme::R_CARD))
            .bg(tokens::media_fill())
            .overflow_hidden()
            .cursor_pointer();

        if media::is_spoiler(attachment) && !self.shown.contains(&attachment.file.id) {
            let id = attachment.file.id.clone();
            return tile
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .type_small()
                        .text_color(theme::text_faint())
                        .child(if width >= 160. { "Spoiler · click to show" } else { "Spoiler" }),
                )
                .child(edge_line())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shown.insert(id.clone());
                    cx.notify();
                }))
                .into_any_element();
        }

        if self.held_back(&still) {
            let what = match shape {
                MediaShape::Video => "Video",
                MediaShape::Gif => "GIF",
                _ => "Image",
            };
            let bytes = attachment.file.bytes;
            return self.load_box(tile, &still, what, bytes, width >= 160., cx).child(edge_line()).into_any_element();
        }

        let natural = natural(attachment);
        let edge = media::decode_edge(natural, width, height, fit, window.scale_factor());
        let size = media::fetch_size(natural, width, height, fit, window.scale_factor());
        let crop = (fit == Fit::CoverTop).then_some(height / width);
        // Whether the picture moves, and whether its frames are here to move in place.
        let (mut moves, mut frames) = (false, false);
        let tile = match self.still(&still, size, edge, crop, cx) {
            Picture::Ready(image) => {
                // A picture smaller than its box sits in the middle at its own size.
                let (wide, tall) = match fit {
                    Fit::Contain if natural.0 > 0 && natural.1 > 0 => {
                        let scale = (width / natural.0 as f32).min(height / natural.1 as f32).min(1.);
                        ((natural.0 as f32 * scale).round(), (natural.1 as f32 * scale).round())
                    }
                    _ => (width, height),
                };
                let object_fit = if fit == Fit::Contain { ObjectFit::Contain } else { ObjectFit::Cover };
                frames = image.frame_count() > 1;
                moves = self.images.moves(&image);
                tile.child(
                    div().absolute().inset_0().flex().items_center().justify_center().child(
                        div().w(px(wide)).h(px(tall)).flex_none().child(anim::picture(image, object_fit, theme::R_CARD, Play::Inline)),
                    ),
                )
            }
            Picture::Loading => tile.child(in_box(Icon::Image, None)),
            Picture::Failed => {
                let tile = self.failed_box(tile, &still, height >= 100. && width >= 180., cx);
                return tile.child(edge_line()).into_any_element();
            }
        };

        // A video, or a GIF that is really an MP4: what says it moves, and what a click does,
        // are `video`'s (`Pane::moving`, where an inline player plugs in). A GIF that is a GIF
        // plays here in its tile, and opens in the viewer like a picture.
        let id = message.id.clone();
        let tile = match shape {
            MediaShape::Video => self.moving(attachment, tile, cx),
            MediaShape::Gif if media::is_video(attachment) => self.moving(attachment, tile, cx),
            _ => {
                // A GIF says so where it does not play by itself. One held still (the setting
                // is Never, the network sent a still, or there was no room) shows that a click
                // plays it: the viewer does.
                let tile = match (shape == MediaShape::Gif || moves, frames) {
                    (true, false) => tile.child(play_disc()).child(badge("GIF".to_owned()).left(px(6.))),
                    (true, true) if anim::badged(Play::Inline) => tile.child(badge("GIF".to_owned()).left(px(6.))),
                    _ => tile,
                };
                tile.on_click(cx.listener(move |this, _, window, cx| this.open_viewer(&id, index, window, cx)))
            }
        };
        let tile = tile.child(edge_line()).children(more.map(|more| {
            div()
                .absolute()
                .inset_0()
                .rounded(px(theme::R_CARD))
                .bg(gpui::rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .type_headline()
                .text_color(gpui::white())
                .child(format!("+{more}"))
        }));
        tile.into_any_element()
    }

    /// A sticker: its own size, no box, no border, and whatever it leaves clear shows the row.
    fn sticker(&mut self, message: &Rc<Message>, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let attachment = &message.attachments[index];
        let still = attachment.thumb.as_ref().unwrap_or(&attachment.file).clone();
        let edge = media::square_size(128., window.scale_factor());
        let sticker = div().id(("sticker", index)).size(px(128.)).flex_none().relative();
        if self.held_back(&still) {
            return self.load_box(sticker.rounded(px(theme::R_CARD)).bg(tokens::media_fill()), &still, "Sticker", 0, false, cx).into_any_element();
        }
        match self.still(&still, edge, edge, None, cx) {
            Picture::Ready(image) => sticker.child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline)),
            Picture::Loading => sticker.child(in_box(Icon::Image, None)),
            Picture::Failed => self.failed_box(sticker, &still, false, cx),
        }
        .into_any_element()
    }

    fn file_row(&mut self, message: &Rc<Message>, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let attachment = &message.attachments[index];
        let file = attachment.file.clone();
        let on_disk = file.path.clone();
        let fetching = self.then.contains_key(&file.id);
        let failed = file.failed.is_some();
        let reason = file.failed.clone();
        let mut meta = media::type_label(attachment);
        if file.bytes > 0 {
            meta = format!("{meta} · {}", media::size_label(file.bytes));
        }
        let (meta, tone) = match (failed, fetching, on_disk.is_some()) {
            (true, ..) => ("Couldn't download · Retry".to_owned(), theme::danger()),
            (_, true, _) => (format!("{meta} · Downloading…"), theme::text_faint()),
            (_, _, true) => (format!("{meta} · Downloaded"), theme::text_faint()),
            _ => (meta, theme::text_faint()),
        };
        let look = attachment.clone();
        let row = div()
            .id(("file", index))
            .w(px(360.))
            .max_w_full()
            .h(px(56.))
            .pl(px(10.))
            .pr(px(8.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when_some(reason, |row, reason| row.tooltip(tip(format!("{reason} Click to try again."), None)))
            .on_click(cx.listener(move |this, _, _, cx| this.fetch_then(&look, Then::QuickLook, cx)))
            .child(
                div()
                    .size(px(36.))
                    .flex_none()
                    .rounded(px(theme::R_CARD))
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(media::file_icon(attachment), 18., theme::text_soft())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().type_ui().font_weight(theme::MEDIUM).child(attachment.name.clone()))
                    .child(div().truncate().type_micro().font_weight(theme::REGULAR).text_color(tone).child(meta)),
            );
        let row = match on_disk {
            Some(path) => {
                let (open, reveal) = (attachment.clone(), path);
                row.child(
                    tool("open", Icon::External, 28.)
                        .tooltip(tip("Open", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.fetch_then(&open, Then::Open, cx);
                        })),
                )
                .child(
                    tool("reveal", Icon::Folder, 28.)
                        .tooltip(tip(crate::host::REVEAL, None))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.reveal_path(std::path::Path::new(&reveal));
                        })),
                )
            }
            None => row.child(tool("download", Icon::Download, 28.).tooltip(tip("Download", None))),
        };
        row.into_any_element()
    }

    /// What a bot or app posts instead of text: one framed card, its bar in one of our colors.
    fn app_card(&self, card: &Card) -> AnyElement {
        let bar = match media::card_tone(card.color) {
            media::Tone::Positive => theme::positive(),
            media::Tone::Danger => theme::danger(),
            media::Tone::Warning => theme::warning(),
            media::Tone::Neutral => tokens::border_strong(),
        };
        let field = |(label, value): &(String, String)| {
            div()
                .w(px(190.))
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().type_small().text_color(theme::text_faint()).child(label.clone()))
                .child(div().type_ui().child(value.clone()))
        };
        let words = div()
            .flex_1()
            .min_w_0()
            .py(px(10.))
            .px(px(12.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .children((!card.title.is_empty()).then(|| div().type_body().font_weight(theme::SEMIBOLD).child(card.title.clone())))
            .children((!card.text.is_empty()).then(|| div().type_ui().text_color(theme::text_soft()).child(card.text.clone())))
            .children((!card.fields.is_empty()).then(|| div().mt(px(2.)).flex().flex_wrap().gap_x(px(12.)).gap_y(px(6.)).children(card.fields.iter().map(field))))
            .children((!card.footer.is_empty()).then(|| div().mt(px(2.)).type_small().text_color(theme::text_faint()).child(card.footer.clone())));
        let framed = div()
            .w(px(440.))
            .max_w_full()
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .flex()
            .child(div().w(px(3.)).flex_none().bg(bar))
            .child(words);
        div().pt(px(5.)).flex().child(framed).into_any_element()
    }

    // Link cards.

    fn cards(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let previews: Vec<Preview> = message.previews.iter().take(CARDS).cloned().collect();
        previews
            .iter()
            .enumerate()
            .map(|(ix, preview)| {
                let card = self.card(ix, preview, window, cx);
                div().pt(px(if ix == 0 { 5. } else { 4. })).flex().child(card).into_any_element()
            })
            .collect()
    }

    fn card(&mut self, index: usize, preview: &Preview, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.wikipedia_preview(preview, cx);
        let scale = window.scale_factor();
        let image = preview.image.as_ref();
        // Only the first card may be large, and only for a picture wide enough to deserve it.
        let large = index == 0
            && image.is_some_and(|image| image.width >= 400 && image.width as f32 >= image.height as f32 * 1.3);
        let words = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children((!preview.site.is_empty()).then(|| {
                div().truncate().type_micro().font_weight(theme::REGULAR).text_color(theme::text_faint()).child(preview.site.clone())
            }))
            .children((!preview.title.is_empty()).then(|| {
                div().line_clamp(2).type_body().font_weight(theme::SEMIBOLD).child(preview.title.clone())
            }))
            .children((!preview.description.is_empty()).then(|| {
                div().line_clamp(2).type_ui().text_color(theme::text_soft()).child(preview.description.clone())
            }));
        let url = preview.url.clone();
        let card = div()
            .id(("card", index))
            .w(px(440.))
            .max_w_full()
            .px(px(12.))
            .py(px(10.))
            .rounded(px(theme::R_FLOAT))
            .bg(tokens::card())
            .border_1()
            .border_color(theme::hairline())
            .cursor_pointer()
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)));
        let Some(image) = image.cloned() else {
            return card.child(words).into_any_element();
        };
        let holder = |id: &'static str, wide: f32, tall: f32, radius: f32| {
            div().id(id).relative().w(px(wide)).h(px(tall)).flex_none().rounded(px(radius)).bg(theme::hover()).overflow_hidden()
        };
        let shown = |image, radius: f32| anim::picture(image, ObjectFit::Cover, radius, Play::Inline);
        if large {
            let wide = self.media_width().min(416.);
            let tall = (wide * image.height as f32 / image.width.max(1) as f32).round().min(210.);
            let edge = media::decode_edge((image.width, image.height), wide, tall, Fit::Cover, scale);
            if self.held_back(&image) {
                let picture = self.load_box(holder("preview", wide, tall, 8.), &image, "Preview", image.bytes, true, cx);
                return card.flex().flex_col().gap(px(8.)).child(words).child(picture).into_any_element();
            }
            let picture = match self.still(&image, edge, edge, None, cx) {
                Picture::Ready(decoded) => holder("preview", wide, tall, 8.).child(shown(decoded, 8.)),
                Picture::Loading => holder("preview", wide, tall, 8.).child(in_box(Icon::Image, None)),
                Picture::Failed => self.failed_box(holder("preview", wide, tall, 8.), &image, false, cx),
            };
            return card.flex().flex_col().gap(px(8.)).child(words).child(picture).into_any_element();
        }
        let edge = media::decode_edge((image.width, image.height), 72., 72., Fit::Cover, scale);
        if self.held_back(&image) {
            let picture = self.load_box(holder("preview", 72., 72., 6.), &image, "Preview", 0, false, cx);
            return card.flex().gap(px(12.)).child(words).child(picture).into_any_element();
        }
        let picture = match self.still(&image, edge, edge, None, cx) {
            Picture::Ready(decoded) => holder("preview", 72., 72., 6.).child(shown(decoded, 6.)),
            Picture::Loading => holder("preview", 72., 72., 6.).child(in_box(Icon::Image, None)),
            Picture::Failed => self.failed_box(holder("preview", 72., 72., 6.), &image, false, cx),
        };
        card.flex().gap(px(12.)).child(words).child(picture).into_any_element()
    }

    // Around the message.

    fn reactions(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.reactions.is_empty() {
            return None;
        }
        let edge = media::square_size(18., window.scale_factor());
        let acts = self.can(message).act;
        let chip = |id: ElementId, mine: bool| {
            let chip = div()
                .id(id)
                .flex_none()
                .h(px(26.))
                .px(px(8.))
                .rounded_full()
                .border_1()
                .flex()
                .items_center()
                .gap(px(5.))
                .when(acts, |chip| chip.cursor_pointer());
            if mine {
                chip.bg(theme::accent_soft()).border_color(theme::accent())
            } else {
                chip.bg(theme::raised()).border_color(gpui::transparent_black()).hover(|style| style.border_color(tokens::border_strong()))
            }
        };
        let mut row = div().mt(px(6.)).mb(px(3.)).flex().flex_wrap().gap(px(6.));
        for (ix, reaction) in message.reactions.iter().take(CHIPS).enumerate() {
            let picture = match &reaction.file {
                Some(file) => match self.still(file, edge, edge, None, cx) {
                    Picture::Ready(image) => Some(div().size(px(18.)).flex_none().child(anim::picture(image, ObjectFit::Contain, 0., Play::Inline)).into_any_element()),
                    _ => None,
                },
                None => None,
            };
            // A custom emoji is its name until its image is here.
            let emoji = picture.unwrap_or_else(|| {
                div().text_size(px(15.)).line_height(px(20.)).child(emoji::display(&reaction.emoji)).into_any_element()
            });
            let count = div()
                .type_small()
                .font_weight(theme::MEDIUM)
                .text_color(if reaction.mine { theme::accent_text() } else { theme::text_soft() })
                .child(reaction.count.to_string());
            // A custom one goes by its file, which names it where its `:name:` may not.
            let toggled = reaction.file.as_ref().map_or_else(|| reaction.emoji.clone(), |file| file.id.clone());
            let (id, on) = (message.id.clone(), !reaction.mine);
            // Joining one the conversation no longer takes, or past a limit, is not offered:
            // the chip says why instead.
            let refused = (!reaction.mine).then(|| self.refusal(message, &toggled)).flatten();
            let words = match (&refused, reaction.mine) {
                (Some(why), _) => why.clone(),
                (None, true) => "Take back your reaction".to_owned(),
                (None, false) => "React with this too".to_owned(),
            };
            let joins = acts && refused.is_none();
            row = row.child(
                chip(("chip", ix).into(), reaction.mine)
                    .child(emoji)
                    .child(count)
                    .when(acts, |chip| chip.tooltip(tip(words, None)))
                    .when(joins, |chip| chip.on_click(cx.listener(move |this, _, _, cx| this.react(&id, &toggled, on, cx))))
                    .when(!joins, |chip| chip.cursor_default()),
            );
        }
        let hidden = message.reactions.len().saturating_sub(CHIPS);
        if hidden > 0 {
            row = row.child(chip("chips-more".into(), false).type_small().text_color(theme::text_soft()).child(format!("+{hidden}")));
        }
        // Always laid out and shown on hover only, so hovering a row never changes its height.
        // Where nothing more may be added it stays laid out and never shows.
        let id = message.id.clone();
        let adds = acts && self.adds(message);
        let add = chip("chip-add".into(), false)
            .opacity(0.)
            .when(adds, |chip| chip.group_hover("row", |style| style.opacity(1.)))
            .child(icon(Icon::SmilePlus, 16., theme::text_soft()))
            .when(adds, |chip| {
                chip.tooltip(tip("Add reaction", None)).on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.open_picker(&id, event.position(), window, cx);
                }))
            });
        Some(row.child(add).into_any_element())
    }

    fn reply_line(&mut self, reply: &protocol::ReplyTo, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics;
        let target = reply.id.clone();
        let gone = reply.deleted || (reply.sender.is_empty() && reply.snippet.is_empty());
        let snippet = match (gone, reply.snippet.is_empty()) {
            (true, _) => "Original message was deleted".to_owned(),
            (false, true) => "Attachment".to_owned(),
            (false, false) => reply.snippet.replace('\n', " "),
        };
        let face = (!gone).then(|| {
            let (_, file) = reply.person.as_ref().map(|person| self.who(person, cx)).unwrap_or_default();
            let plate = match &reply.person {
                Some(person) => Avatar::new(person, &reply.sender, 16.),
                None => Avatar::named(&reply.sender, 16.),
            };
            self.face(plate, file.as_ref(), 16., Shape::Person, window, cx)
        });
        // The elbow runs from the middle of the avatar column up and across to the quote.
        let reach = metrics.avatar / 2. + metrics.gap - 4.;
        div()
            .id("reply")
            .h(px(20.))
            .mb(px(2.))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::text_faint())
            .when(!gone, |line| {
                line.cursor_pointer()
                    .hover(|style| style.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(&target, cx)))
            })
            .child(
                div().w(px(metrics.avatar + metrics.gap - 6.)).h_full().flex_none().relative().child(
                    div()
                        .absolute()
                        .left(px(metrics.avatar / 2. - 1.))
                        .top(px(9.))
                        .w(px(reach))
                        .h(px(11.))
                        .border_l_2()
                        .border_t_2()
                        .border_color(tokens::border_strong())
                        .rounded_tl(px(6.)),
                ),
            )
            .children(face)
            .children((!gone).then(|| {
                div().flex_none().font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child(reply.sender.clone())
            }))
            .child(div().min_w_0().truncate().when(gone, |snippet| snippet.italic()).child(snippet))
    }

    fn thread_line(&mut self, message: &Rc<Message>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A thread does not open inside a thread.
        let thread = message.thread.as_ref().filter(|_| !self.is_thread())?;
        let replies = if thread.replies == 1 { "1 reply".to_owned() } else { format!("{} replies", thread.replies) };
        let last = match age(now(), thread.last).as_str() {
            "now" => "Last reply just now".to_owned(),
            ago => format!("Last reply {ago} ago"),
        };
        // The latest repliers, overlapped, each ringed in the color behind them.
        // Placed by hand: each a ringed 24 pt disc, 19 pt on from the one before.
        let shown = thread.participants.len().min(protocol::THREAD_FACES);
        let mut faces = div().flex_none().relative().h(px(24.)).w(px(24. + 19. * shown.saturating_sub(1) as f32));
        for (ix, person) in thread.participants.iter().take(shown).enumerate() {
            let (name, file) = self.who(person, cx);
            let face = self.face(Avatar::new(person, &name, 20.), file.as_ref(), 20., Shape::Person, window, cx);
            faces = faces.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(19. * ix as f32))
                    .rounded_full()
                    .border_2()
                    .border_color(theme::canvas())
                    .child(face),
            );
        }
        let unread = (thread.unread > 0).then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .type_small()
                .text_color(theme::accent_text())
                .child(div().size(px(6.)).rounded_full().bg(theme::accent()))
                .child(format!("{} new", thread.unread))
        });
        let root = message.clone();
        let line = div()
            .id("thread")
            .h(px(30.))
            .mt(px(2.))
            .ml(px(-8.))
            .px(px(8.))
            .rounded(px(theme::R_CARD))
            .border_1()
            .border_color(gpui::transparent_black())
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised()).border_color(theme::hairline()))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PaneEvent::OpenThread(root.clone()))))
            .when(thread.participants.is_empty(), |line| line.child(icon(Icon::Thread, 14., theme::accent_text())))
            .when(!thread.participants.is_empty(), |line| line.child(faces))
            .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::accent_text()).child(replies))
            .child(div().type_small().text_color(theme::text_faint()).child(last))
            .children(unread);
        Some(div().flex().child(line).into_any_element())
    }

    /// Under one of my messages still going: "Sending…", or for files the bar of what has
    /// gone, moved only by chatd's `Upload` changes, and Cancel.
    fn sending(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if message.state != SendState::Pending {
            return None;
        }
        let client_id = message.client_id.clone()?;
        let words = div().type_small().text_color(theme::text_faint()).font_family(theme::FONT);
        let Some(&(sent, total)) = self.uploads.get(&client_id) else {
            return Some(div().h(px(18.)).flex().items_center().child(words.child("Sending…")).into_any_element());
        };
        let conversation = self.key()?.clone();
        let done = if total == 0 { 0. } else { (sent as f32 / total as f32).clamp(0., 1.) };
        let bar = div()
            .flex_none()
            .w(px(240.))
            .h(px(4.))
            .rounded(px(2.))
            .bg(gpui::rgba(0xffffff1a))
            .child(div().h_full().w(px(240. * done)).rounded(px(2.)).bg(theme::accent()));
        let cancel = div()
            .id("cancel-upload")
            .flex_none()
            .size(px(20.))
            .rounded(px(theme::R_CHIP))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()))
            .tooltip(tip("Cancel upload", None))
            .child(icon(Icon::Close, 12., theme::text_soft()))
            .on_click(cx.listener(move |_, _, _, cx| {
                let request = protocol::Request::CancelSend { conversation: conversation.clone(), client_id: client_id.clone() };
                cx.background_executor().spawn(async move { crate::client::ask(&request) }).detach();
            }));
        let sizes: Vec<u64> = message.attachments.iter().map(|attachment| attachment.file.bytes).collect();
        let line = div()
            .h(px(20.))
            .mt(px(4.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(bar)
            .child(words.child(uploading_words(&sizes, sent, total)))
            .children(silent_mark(message))
            .child(cancel);
        Some(line.into_any_element())
    }

    /// What a message that did not go says under its body: the network's reason, then the two
    /// things that can be done about it, as words.
    fn unsent(&self, message: &Rc<Message>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let SendState::Failed { reason } = &message.state else {
            return None;
        };
        let key = self.key()?.clone();
        let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
        let (again, drop) = ((key.clone(), client_id.clone()), (key, client_id));
        let line = div()
            .h(px(18.))
            .flex()
            .items_center()
            .gap(px(6.))
            .type_small()
            .text_color(theme::danger())
            .child(icon(Icon::Alert, 13., theme::danger()))
            .child(div().min_w_0().truncate().child(format!("Not sent · {reason}")))
            .child(
                div()
                    .id("retry")
                    .flex_none()
                    .ml(px(4.))
                    .font_weight(theme::SEMIBOLD)
                    .text_color(tokens::link())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("Retry")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (conversation, client_id) = again.clone();
                        let _ = this.shell.update(cx, |shell, cx| shell.retry_send(conversation, client_id, cx));
                    })),
            )
            .child(
                div()
                    .id("discard")
                    .flex_none()
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (conversation, client_id) = drop.clone();
                        let _ = this.shell.update(cx, |shell, cx| shell.discard_send(conversation, client_id, cx));
                    })),
            );
        Some(line.into_any_element())
    }
}

/// Bytes as the upload line says them: "3.1 MB", "820 KB".
/// "Uploading 2 of 3 · 3.1 of 5.4 MB" while a message's files go, "Uploading · …" for one.
fn uploading_words(sizes: &[u64], sent: u64, total: u64) -> String {
    let amount = format!("{} of {}", megabytes(sent), megabytes(total));
    if sizes.len() < 2 {
        return format!("Uploading · {amount}");
    }
    // The file under way: the first whose end the bytes sent have not reached.
    let mut end = 0;
    let at = sizes.iter().position(|bytes| {
        end += bytes;
        sent < end
    });
    format!("Uploading {} of {} · {amount}", at.map_or(sizes.len(), |at| at + 1), sizes.len())
}

/// The bell-off after a message's time when it went without notification.
fn silent_mark(message: &Message) -> Option<AnyElement> {
    let client_id = message.client_id.as_deref()?;
    crate::gui::composer::sent_silently(client_id).then(|| {
        div()
            .id(name_id("silent", client_id))
            .flex_none()
            .flex()
            .items_center()
            .tooltip(tip("Sent without notification", None))
            .child(glyph("icons/bell-off.svg", 12., theme::text_faint()))
            .into_any_element()
    })
}

fn megabytes(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1000)),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.),
    }
}

/// Where a forwarded message was first said.
fn forwarded_line(from: &Forwarded) -> Div {
    let place = match (from.from_title.is_empty(), from.from_space.is_empty()) {
        (true, true) => "Forwarded".to_owned(),
        (false, true) => format!("Forwarded from {}", from.from_title),
        (true, false) => format!("Forwarded from {}", from.from_space),
        (false, false) => format!("Forwarded from {} in {}", from.from_title, from.from_space),
    };
    div()
        .h(px(18.))
        .mb(px(2.))
        .flex()
        .items_center()
        .gap(px(5.))
        .type_small()
        .text_color(theme::text_faint())
        .child(icon(Icon::Forward, 13., theme::text_faint()))
        .child(div().min_w_0().truncate().child(place))
}

/// The size the network gave for an attachment's picture: the file's, or its still's.
fn natural(attachment: &Attachment) -> (u32, u32) {
    let still = attachment.thumb.as_ref().unwrap_or(&attachment.file);
    if attachment.file.width > 0 && attachment.file.height > 0 {
        (attachment.file.width, attachment.file.height)
    } else {
        (still.width, still.height)
    }
}

/// The message that says most of who someone is here: their newest with a picture, else their
/// newest. A tombstone keeps the name and loses the picture, so one that has it is looked for first.
fn said_by<'a>(root: Option<&'a Rc<Message>>, messages: &'a [Rc<Message>], id: &str) -> Option<&'a Message> {
    let theirs = || root.into_iter().chain(messages).rev().filter(|message| message.sender.id == id);
    let found = theirs().find(|message| message.sender.avatar.is_some()).or_else(|| theirs().next());
    found.map(|message| &**message)
}

/// The picture on a message of mine: the one it names, else the account's, which follows a
/// change of picture. One I am sending is not from the network yet and names none. Without the
/// account's (a network that keeps no row of me), my newest message here lends the picture it
/// was stored with, which may since have changed.
fn own_face(
    message: &Message,
    root: Option<&Rc<Message>>,
    messages: &[Rc<Message>],
    me: &str,
    account: impl FnOnce() -> Option<MediaFile>,
) -> Option<MediaFile> {
    let shown = || match me {
        // Without my id nothing here is known to be mine.
        "" => None,
        me => said_by(root, messages, me)?.sender.avatar.clone(),
    };
    message.sender.avatar.clone().or_else(account).or_else(shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaKind;

    fn picture(id: &str) -> MediaFile {
        MediaFile::new(id, MediaKind::Avatar)
    }

    /// One of mine the network has, with the picture it was stored with.
    fn mine(id: &str, avatar: Option<&str>) -> Rc<Message> {
        let mut message = Message::plain(id, "Jo", "said", 10, true);
        message.sender.id = "me".into();
        message.sender.avatar = avatar.map(picture);
        Rc::new(message)
    }

    /// One of mine still going: chatd names no sender on it.
    fn sending() -> Message {
        let mut message = Message::plain("c1", "You", "going", 20, true);
        message.state = SendState::Pending;
        message
    }

    fn face(message: &Message, messages: &[Rc<Message>], account: Option<&str>) -> Option<String> {
        own_face(message, None, messages, "me", || account.map(picture)).map(|file| file.id)
    }

    #[test]
    fn my_message_shows_its_own_picture_then_the_accounts_then_my_newest_here() {
        let messages = [mine("1", Some("shown-here"))];
        assert_eq!(face(&mine("2", Some("its-own")), &messages, Some("account")).as_deref(), Some("its-own"));
        // One being sent names none.
        assert_eq!(face(&sending(), &messages, Some("account")).as_deref(), Some("account"));
        assert_eq!(face(&sending(), &messages, None).as_deref(), Some("shown-here"));
        assert_eq!(face(&sending(), &[], None), None);
    }

    #[test]
    fn a_picture_i_have_since_changed_does_not_come_back_from_my_earlier_messages() {
        let messages = [mine("1", Some("before")), mine("2", Some("before"))];
        assert_eq!(face(&sending(), &messages, Some("after")).as_deref(), Some("after"));
    }

    #[test]
    fn only_my_newest_picture_here_is_lent_and_only_once_my_id_is_known() {
        // A newer one of mine without a picture, a tombstone, does not lose it.
        let messages = [mine("1", Some("older")), mine("2", Some("newer")), mine("3", None)];
        assert_eq!(face(&sending(), &messages, None).as_deref(), Some("newer"));
        let mut theirs = Message::plain("4", "mara", "hi", 30, false);
        theirs.sender.id = "mara".into();
        theirs.sender.avatar = Some(picture("mara"));
        assert_eq!(face(&sending(), &[Rc::new(theirs)], None), None);
        assert_eq!(own_face(&sending(), None, &messages, "", || None), None);
    }
}
