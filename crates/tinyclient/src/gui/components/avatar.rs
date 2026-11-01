//! Avatars: a circle is a person, a rounded square is a place. An image when its file is on
//! disk, decoded off the main thread at the size it is drawn; a flat tint with initials until then.
//! A moving one (Discord's `a_` avatars and server icons) plays under the pointer in a list, as
//! Discord's own lists do, and as the autoplay setting says where it is asked to.

use super::glyph;
use crate::frames;
use crate::gui::anim::{self, Play};
use crate::gui::theme;
use gpui::{canvas, div, prelude::*, px, App, Asset, Div, Global, ObjectFit, RenderImage, Rgba, Window};
use image::{imageops::FilterType, Frame, ImageReader};
use protocol::{Key, MediaFile, Presence};
use smallvec::SmallVec;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

/// Decoded avatars kept. Far more than one screen shows; the rest are decoded again on demand.
const THUMBS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Person,
    Place,
}

/// A file on disk, how many pixels across it is wanted, and whether its frames are.
type ThumbSource = (PathBuf, u32, bool);

/// The largest avatar file read whole to look for frames.
const MOST_READ: u64 = 4 << 20;

struct Thumb;

impl Asset for Thumb {
    type Source = ThumbSource;
    type Output = Option<Arc<RenderImage>>;

    fn load(
        (path, pixels, moving): Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        cx.background_executor().spawn(async move { moving.then(|| frames_of(&path, pixels)).flatten().or_else(|| decode(&path, pixels)) })
    }
}

/// A moving avatar's frames, when it moves and there is room in [`anim::AVATARS`].
fn frames_of(path: &std::path::Path, pixels: u32) -> Option<Arc<RenderImage>> {
    let bytes = std::fs::metadata(path).ok().filter(|meta| meta.len() <= MOST_READ).and_then(|_| std::fs::read(path).ok())?;
    let animation = frames::decode(&bytes, pixels, None, frames::SMALL)?;
    if !anim::AVATARS.take(animation.bytes()) {
        return None;
    }
    anim::render_image(animation)
}

fn decode(path: &std::path::Path, pixels: u32) -> Option<Arc<RenderImage>> {
    let image = ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let mut scaled = image.resize_to_fill(pixels, pixels, FilterType::CatmullRom).into_rgba8();
    // GPUI's textures are BGRA.
    for pixel in scaled.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frames: SmallVec<[Frame; 1]> = SmallVec::from_elem(Frame::new(scaled), 1);
    Some(Arc::new(RenderImage::new(frames)))
}

/// Which thumbs are decoded, oldest first, so the cache stays bounded.
#[derive(Default)]
struct Thumbs {
    known: HashSet<ThumbSource>,
    order: VecDeque<ThumbSource>,
}

impl Global for Thumbs {}

fn thumb(source: &ThumbSource, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    if !cx.default_global::<Thumbs>().known.contains(source) {
        let thumbs = cx.global_mut::<Thumbs>();
        thumbs.known.insert(source.clone());
        thumbs.order.push_back(source.clone());
        if thumbs.order.len() > THUMBS {
            if let Some(old) = thumbs.order.pop_front() {
                thumbs.known.remove(&old);
                if let Some(Some(image)) = window.use_asset::<Thumb>(&old, cx) {
                    if image.frame_count() > 1 {
                        anim::AVATARS.give(anim::bytes_of(&image));
                        anim::forget(&image);
                    }
                    cx.drop_image(image, Some(window));
                }
                cx.remove_asset::<Thumb>(&old);
            }
        }
    }
    window.use_asset::<Thumb>(source, cx).flatten()
}

/// The picture at `path` filling a square of `size` points, decoded off the main thread at that
/// size and kept in the avatars' cache: an avatar's picture, or a tile of shared media.
pub fn square(path: PathBuf, size: f32, radius: f32, play: Play) -> gpui::Canvas<()> {
    let source: ThumbSource = (path, (size * 2.).ceil() as u32, anim::wants_frames(play));
    // Asked for while painting, so a picture that lands draws the view again.
    canvas(|_, _, _| (), move |bounds, (), window, cx| {
        if let Some(image) = thumb(&source, window, cx) {
            anim::paint(bounds, &image, ObjectFit::Cover, px(radius), play, window, cx);
        }
    })
}

/// Up to `count` letters that stand for a name.
pub fn initials(name: &str, count: usize) -> String {
    let letters: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .take(count)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters
    }
}

fn hash(parts: &[&str]) -> u32 {
    parts.iter().flat_map(|part| part.bytes().chain([0])).fold(2166136261u32, |hash, byte| {
        (hash ^ byte as u32).wrapping_mul(16777619)
    })
}

/// The tint of a person or place: hashed from the network and id, so it is the same everywhere.
pub fn tint_of(key: &Key) -> usize {
    (hash(&[key.network.label(), &key.id]) % 8) as usize
}

/// A tint the conversation names itself (Telegram's topic color): over the hash when there.
pub fn tint_from(key: &Key, icon: Option<i32>) -> usize {
    icon.filter(|icon| *icon >= 0 && *icon < 8).map(|icon| icon as usize).unwrap_or_else(|| tint_of(key))
}

pub struct Avatar {
    name: String,
    tint: usize,
    size: f32,
    shape: Shape,
    image: Option<PathBuf>,
    glyph: Option<&'static str>,
    /// A glyph plate takes the tint rather than the neutral fill.
    tinted: bool,
    presence: Option<(Presence, Rgba)>,
    /// How many letters stand in for a missing picture; by size when unset.
    letters: Option<usize>,
    /// When a moving picture plays: under the pointer, unless asked otherwise.
    play: Play,
}

impl Avatar {
    pub fn new(key: &Key, name: &str, size: f32) -> Avatar {
        Avatar {
            name: name.to_owned(),
            tint: tint_of(key),
            size,
            shape: Shape::Person,
            image: None,
            glyph: None,
            tinted: false,
            presence: None,
            letters: None,
            play: Play::Hover,
        }
    }

    /// With no id to hash, the name stands in.
    pub fn named(name: &str, size: f32) -> Avatar {
        Avatar {
            name: name.to_owned(),
            tint: (hash(&[name]) % 8) as usize,
            size,
            shape: Shape::Person,
            image: None,
            glyph: None,
            tinted: false,
            presence: None,
            letters: None,
            play: Play::Hover,
        }
    }

    /// A tint the conversation names itself, over the id's hash.
    pub fn tint(mut self, tint: usize) -> Avatar {
        self.tint = tint;
        self
    }

    /// A channel: its glyph on a neutral plate.
    pub fn channel(icon: &'static str, size: f32) -> Avatar {
        Avatar {
            glyph: Some(icon),
            shape: Shape::Place,
            ..Avatar::named("", size)
        }
    }

    /// A channel in a list that mixes Spaces: its glyph on its Space's tint, so the channels
    /// of one Space read as one family and two Spaces apart.
    pub fn channel_in(space: &Key, icon: &'static str, size: f32) -> Avatar {
        Avatar { glyph: Some(icon), shape: Shape::Place, tinted: true, ..Avatar::new(space, "", size) }
    }

    /// A group's letters: two at every size, so its initials read the same in every list.
    pub fn letters(mut self, count: usize) -> Avatar {
        self.letters = Some(count);
        self
    }

    /// Where a moving picture plays as messages do, not only under the pointer: a profile.
    pub fn playing(mut self, play: Play) -> Avatar {
        self.play = play;
        self
    }

    pub fn shape(mut self, shape: Shape) -> Avatar {
        self.shape = shape;
        self
    }

    /// Drawn once the file is on disk; the tint holds its box until then.
    pub fn image(mut self, file: Option<&MediaFile>) -> Avatar {
        self.image = file.and_then(|file| file.path.clone()).map(PathBuf::from);
        self
    }

    /// The picture's file, when the caller has already found where it is.
    pub fn image_at(mut self, path: Option<PathBuf>) -> Avatar {
        self.image = path;
        self
    }

    /// `ring` is the color behind the avatar, so the dot's ring follows hover and selection.
    #[allow(dead_code)]
    pub fn presence(mut self, presence: Presence, ring: Rgba) -> Avatar {
        self.presence = Some((presence, ring));
        self
    }

    pub fn build(self) -> Div {
        let size = self.size;
        let radius = match self.shape {
            Shape::Person => size / 2.,
            Shape::Place => (size * 0.25).round(),
        };
        let (fill, letters) = theme::tint(self.tint);
        let plate = div().size(px(size)).rounded(px(radius)).flex().items_center().justify_center();
        let plate = match self.glyph {
            Some(icon) if self.tinted => plate.bg(fill).child(glyph(icon, if size >= 32. { 16. } else { 14. }, letters)),
            Some(icon) => plate
                .bg(theme::raised())
                .child(glyph(icon, if size >= 32. { 16. } else { 14. }, theme::text_soft())),
            None => plate
                .bg(fill)
                .text_color(letters)
                .text_size(px((size * 0.4).round()))
                .font_weight(theme::SEMIBOLD)
                .child(initials(&self.name, self.letters.unwrap_or(if size >= 32. { 2 } else { 1 }))),
        };
        let picture = self.image.map(|path| square(path, size, radius, self.play).absolute().top_0().left_0().size(px(size)));
        div()
            .relative()
            .size(px(size))
            .flex_none()
            .child(plate)
            .children(picture)
            // Keeps a dark image off the dark surface.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(size))
                    .rounded(px(radius))
                    .border_1()
                    .border_color(gpui::rgba(0xffffff0f)),
            )
            .children(self.presence.and_then(|(presence, ring)| presence_dot(presence, size, ring)))
    }
}

/// The two people a group's picture shows: the first two of `names` when it lists two or more
/// (`Ana, Ben, +3`, or members the caller knows). A group with a name of its own lists none.
pub fn group_faces(names: &str) -> Option<(&str, &str)> {
    let mut names = names.split(',').map(str::trim).filter(|name| !name.is_empty() && !name.starts_with('+'));
    Some((names.next()?, names.next()?))
}

/// A group conversation with no picture of its own: two of its people at 66% on the diagonal,
/// the front one ringed in the color behind the avatar, when `names` names two of them; else
/// the two letters of its title (`group_faces`). The one look of a group, at every size.
// CONTRACT: a group conversation does not list its members, so callers that know none pass the
// title, which the networks build from the members when the group has no name.
pub fn pair(key: &Key, names: &str, size: f32, ring: Rgba) -> Div {
    pair_or(key, names, names, size, ring, Shape::Person)
}

/// [`pair`] with the title for the letters and the shape of the letters' plate (Telegram's
/// groups are places).
pub fn pair_or(key: &Key, names: &str, title: &str, size: f32, ring: Rgba, shape: Shape) -> Div {
    let Some((back, front)) = group_faces(names) else {
        return Avatar::new(key, title, size).letters(2).shape(shape).build();
    };
    let small = (size * 0.66).round();
    let person = |name: &str| Avatar::new(&Key { id: format!("{}/{name}", key.id), ..key.clone() }, name, small).build();
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .child(div().absolute().top_0().left_0().child(person(back)))
        .child(
            div()
                .absolute()
                .right(px(-2.))
                .bottom(px(-2.))
                .rounded(px(small / 2. + 2.))
                .border_2()
                .border_color(ring)
                .child(person(front)),
        )
}

/// The presence shape on an avatar's bottom-right corner, coded by shape as well as color.
/// Unknown draws nothing, and so does a 16 pt avatar.
pub fn presence_dot(presence: Presence, avatar: f32, ring: Rgba) -> Option<Div> {
    let diameter = match avatar as u32 {
        0..=19 => return None,
        20..=31 => 8.,
        32..=63 => 10.,
        _ => 14.,
    };
    let dot = div().size(px(diameter)).rounded(px(diameter / 2.));
    let dot = match presence {
        Presence::Unknown => return None,
        Presence::Online => dot.bg(theme::online()),
        // A bite out of the top-left.
        Presence::Idle => dot.bg(theme::idle()).relative().child(
            div()
                .absolute()
                .top(px(-1.))
                .left(px(-1.))
                .size(px((diameter * 0.6).round()))
                .rounded(px(diameter))
                .bg(ring),
        ),
        Presence::Busy => dot.bg(theme::dnd()),
        Presence::Dnd => dot.bg(theme::dnd()).flex().items_center().justify_center().child(
            div().w(px((diameter * 0.6).round())).h(px(2.)).rounded(px(1.)).bg(ring),
        ),
        Presence::Offline => dot.border_2().border_color(theme::offline()),
    };
    let outer = diameter + 4.;
    Some(
        div()
            .absolute()
            .right(px(-4.))
            .bottom(px(-4.))
            .size(px(outer))
            .rounded(px(outer / 2.))
            .bg(ring)
            .flex()
            .items_center()
            .justify_center()
            .child(dot),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_shows_two_faces_only_when_it_names_two_people_else_its_two_letters() {
        assert_eq!(group_faces("Ana, Ben, +3"), Some(("Ana", "Ben")));
        assert_eq!(group_faces("Kofi Asante, Mirela Dragan, Wren Halloran"), Some(("Kofi Asante", "Mirela Dragan")));
        // A group with a name of its own: no faces, so its letters, the same at every size.
        assert_eq!(group_faces("Foraging Lisbon"), None);
        assert_eq!(group_faces("Ana, +3"), None);
        assert_eq!(initials("Foraging Lisbon", 2), "FL");
        assert_eq!(initials("Family", 2), "F");
    }
}
