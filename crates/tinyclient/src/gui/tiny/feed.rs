//! The picture being watched. mediad writes each frame as a JPEG (pixels would not fit through
//! chatd's frames, which also carry every other change), and each gets a path of its own. Drawn
//! by path, GPUI keeps every one of them until told otherwise, as much as 37 MB a second at
//! 720p. So a frame is read off the main thread into one [`RenderImage`], drawn by that image,
//! and let go of as soon as the next one is in. A frame that comes while one is being read is
//! only remembered, so a window that cannot keep up skips frames and does not queue them.

use crate::gui::state::Shell;
use gpui::{Context, RenderImage};
use image::RgbaImage;
use protocol::Call;
use std::path::Path;
use std::sync::Arc;

/// What the picture's frames are doing, whatever `I` holds a frame.
pub(in crate::gui) struct Feed<I> {
    shown: Option<I>,
    /// The newest frame chatd told of: its generation and file.
    newest: Option<(u64, String)>,
    /// The generation being read now.
    reading: Option<u64>,
}

impl<I> Default for Feed<I> {
    fn default() -> Self {
        Feed { shown: None, newest: None, reading: None }
    }
}

impl<I> Feed<I> {
    pub(in crate::gui) fn shown(&self) -> Option<&I> {
        self.shown.as_ref()
    }

    /// chatd says this is the picture now. What comes back is the file to start reading, and a
    /// frame that came off the screen.
    fn offer(&mut self, picture: Option<(u64, String)>) -> (Option<(u64, String)>, Option<I>) {
        let Some(picture) = picture else {
            self.newest = None;
            return (None, self.shown.take());
        };
        if self.newest.as_ref().is_some_and(|(generation, _)| *generation == picture.0) {
            return (None, None);
        }
        self.newest = Some(picture);
        (self.next(), None)
    }

    /// A read finished. What comes back is the frame it replaced, or itself if nobody wants it
    /// any more, and the next file to read.
    fn landed(&mut self, generation: u64, image: Option<I>) -> (Option<I>, Option<(u64, String)>) {
        self.reading = None;
        let wanted = self.newest.is_some();
        let gone = match image {
            Some(image) if wanted => self.shown.replace(image),
            image => image,
        };
        if self.newest.as_ref().is_some_and(|(newest, _)| *newest == generation) {
            return (gone, None);
        }
        (gone, self.next())
    }

    fn next(&mut self) -> Option<(u64, String)> {
        if self.reading.is_some() {
            return None;
        }
        let newest = self.newest.clone()?;
        self.reading = Some(newest.0);
        Some(newest)
    }
}

/// A JPEG as the BGRA GPUI draws.
fn bgra(jpeg: &[u8]) -> Option<RgbaImage> {
    let mut pixels = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(pixels)
}

/// One frame from its file. mediad removes a file once the next is written, so one the window
/// fell behind on may be gone, and the newer one is on its way.
fn frame(path: &Path) -> Option<Arc<RenderImage>> {
    let pixels = bgra(&std::fs::read(path).ok()?)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

impl Shell {
    /// The call changed: the frame it names is read, and one no longer named is let go.
    pub(in crate::gui) fn feed_follow(&mut self, call: Option<&Call>, cx: &mut Context<Self>) {
        let picture = call.and_then(|call| call.picture.as_ref()).map(|picture| (picture.generation, picture.path.clone()));
        let (read, gone) = self.call_state.feed.offer(picture);
        if let Some(gone) = gone {
            cx.drop_image(gone, None);
            cx.notify();
        }
        if let Some(read) = read {
            self.feed_read(read, cx);
        }
    }

    fn feed_read(&mut self, (generation, path): (u64, String), cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let image = cx.background_executor().spawn(async move { frame(Path::new(&path)) }).await;
            let _ = this.update(cx, |this, cx| {
                let (gone, next) = this.call_state.feed.landed(generation, image);
                if let Some(gone) = gone {
                    cx.drop_image(gone, None);
                }
                cx.notify();
                if let Some(next) = next {
                    this.feed_read(next, cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(generation: u64) -> Option<(u64, String)> {
        Some((generation, format!("{generation}.jpg")))
    }

    #[test]
    fn each_new_frame_lets_go_of_the_one_before_it() {
        let mut feed = Feed::<u64>::default();
        assert_eq!(feed.offer(at(1)), (at(1), None));
        assert_eq!(feed.landed(1, Some(1)), (None, None));
        assert_eq!(feed.shown(), Some(&1));
        assert_eq!(feed.offer(at(2)), (at(2), None));
        assert_eq!(feed.landed(2, Some(2)), (Some(1), None), "frame 1 is released");
        assert_eq!(feed.shown(), Some(&2));
    }

    #[test]
    fn frames_that_come_during_a_read_are_skipped_to_the_newest() {
        let mut feed = Feed::<u64>::default();
        assert_eq!(feed.offer(at(1)), (at(1), None));
        assert_eq!(feed.offer(at(2)), (None, None), "one read at a time");
        assert_eq!(feed.offer(at(3)), (None, None));
        assert_eq!(feed.landed(1, Some(1)), (None, at(3)), "2 was never read");
        assert_eq!(feed.landed(3, Some(3)), (Some(1), None));
    }

    #[test]
    fn a_frame_that_could_not_be_read_keeps_the_one_on_screen() {
        let mut feed = Feed::<u64>::default();
        feed.offer(at(1));
        feed.landed(1, Some(1));
        feed.offer(at(2));
        assert_eq!(feed.landed(2, None), (None, None));
        assert_eq!(feed.shown(), Some(&1));
    }

    #[test]
    fn stopping_releases_the_frame_and_one_still_being_read() {
        let mut feed = Feed::<u64>::default();
        feed.offer(at(1));
        feed.landed(1, Some(1));
        feed.offer(at(2));
        assert_eq!(feed.offer(None), (None, Some(1)));
        assert_eq!(feed.shown(), None);
        assert_eq!(feed.landed(2, Some(2)), (Some(2), None), "nobody wants it any more");
        assert_eq!(feed.shown(), None);
    }

    #[test]
    fn a_jpeg_arrives_as_bgra() {
        let mut jpeg = Vec::new();
        let red = image::RgbImage::from_pixel(16, 16, image::Rgb([250, 0, 0]));
        red.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let pixels = bgra(&jpeg).unwrap();
        let pixel = pixels.get_pixel(8, 8).0;
        assert!(pixel[2] > 200 && pixel[0] < 60, "red is in the third byte: {pixel:?}");
        assert!(bgra(b"not a jpeg").is_none());
    }
}
