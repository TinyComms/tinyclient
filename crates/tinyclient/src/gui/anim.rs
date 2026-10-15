//! Moving pictures in the window: whether one plays, which frame it shows, and when the window
//! draws next. A picture plays only while it is drawn, on screen, in the key window, and as the
//! user's autoplay setting and the system's Reduce Motion allow. Nothing asks for a frame at the
//! display's rate: one timer per window wakes it when the next frame of any playing picture is
//! due, so a 10 fps GIF draws the window 10 times a second and a paused one never.
//!
//! Decoded frames are paid for from one of two [`Budget`]s, so pictures in messages and
//! avatars cannot crowd each other out and neither grows without end.

use super::app;
use gpui::{
    canvas, img, prelude::*, px, AnyElement, App, Bounds, Corners, ImageId, ImageSource, MouseMoveEvent, ObjectFit,
    Pixels, RenderImage, Task, Window, WindowId,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The user's choice, kept with the window's prefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Autoplay {
    /// Every moving picture on screen plays.
    #[default]
    Always,
    /// One plays while the pointer is on it.
    Hover,
    /// None plays in place; opening one plays it.
    Never,
}

impl Autoplay {
    pub fn word(self) -> &'static str {
        match self {
            Autoplay::Always => "always",
            Autoplay::Hover => "hover",
            Autoplay::Never => "never",
        }
    }

    pub fn from_word(word: &str) -> Option<Autoplay> {
        [Autoplay::Always, Autoplay::Hover, Autoplay::Never].into_iter().find(|autoplay| autoplay.word() == word)
    }
}

// One window, one value, read on every draw: a global is the cheapest honest home.
static AUTOPLAY: AtomicU8 = AtomicU8::new(0);

pub fn autoplay() -> Autoplay {
    match AUTOPLAY.load(Ordering::Relaxed) {
        1 => Autoplay::Hover,
        2 => Autoplay::Never,
        _ => Autoplay::Always,
    }
}

/// Takes effect at the next draw. The caller saves the prefs.
pub fn set_autoplay(autoplay: Autoplay) {
    AUTOPLAY.store(autoplay as u8, Ordering::Relaxed);
}

/// Where a moving picture is drawn, which decides when it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Play {
    /// In a message, a link's preview, a reaction, the picker: as the autoplay setting says.
    Inline,
    /// An avatar or a server's icon in a list: only under the pointer, as Discord does, so a
    /// list of them is never a wall of motion.
    Hover,
    /// Opened on purpose (the viewer): it plays, whatever the setting.
    Asked,
}

/// Whether frames are worth decoding here at all: a picture that will never play is held as
/// its first frame alone.
pub fn wants_frames(play: Play) -> bool {
    play == Play::Asked || autoplay() != Autoplay::Never
}

/// Whether a moving picture here wears its GIF badge: it does not play by itself.
pub fn badged(play: Play) -> bool {
    play == Play::Inline && (autoplay() != Autoplay::Always || app::reduce_motion())
}

/// Bytes of decoded frames one kind of picture may hold across the whole window. A picture
/// that does not fit is drawn still until there is room.
pub struct Budget {
    held: AtomicUsize,
    most: usize,
}

/// Pictures in messages, previews and the pickers: four large GIFs at 2x, or many small ones.
pub static MESSAGES: Budget = Budget { held: AtomicUsize::new(0), most: 48 << 20 };
/// Avatars and server icons: a few dozen moving ones at list sizes.
pub static AVATARS: Budget = Budget { held: AtomicUsize::new(0), most: 8 << 20 };

impl Budget {
    /// Takes `bytes` when they fit. Called from decode threads.
    pub fn take(&self, bytes: usize) -> bool {
        self.held
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| (held + bytes <= self.most).then_some(held + bytes))
            .is_ok()
    }

    pub fn give(&self, bytes: usize) {
        let _ = self.held.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| Some(held.saturating_sub(bytes)));
    }

    /// Whether a picture of `bytes` would fit now.
    pub fn room(&self, bytes: usize) -> bool {
        self.held.load(Ordering::Relaxed) + bytes <= self.most
    }

    #[cfg_attr(not(feature = "gui-shot"), allow(dead_code))]
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }
}

/// How many times each decoded animation plays, by image, when not forever. Written by the
/// decode threads, read when drawing.
static PLAYS: Mutex<Option<HashMap<ImageId, u32>>> = Mutex::new(None);

/// Builds the window's image from decoded frames, noting how often it plays.
pub fn render_image(animation: crate::frames::Animation) -> Option<Arc<RenderImage>> {
    let (width, height, plays) = (animation.width, animation.height, animation.plays);
    let frames: Vec<image::Frame> = animation
        .frames
        .into_iter()
        .map(|(bgra, held)| {
            let buffer = image::ImageBuffer::from_raw(width, height, bgra)?;
            Some(image::Frame::from_parts(buffer, 0, 0, image::Delay::from_saturating_duration(held)))
        })
        .collect::<Option<_>>()?;
    let image = Arc::new(RenderImage::new(frames));
    if let Some(plays) = plays {
        PLAYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_or_insert_default().insert(image.id, plays);
    }
    Some(image)
}

/// The first frame alone, to stand in for an animation whose frames are let go.
pub fn first_frame(image: &RenderImage) -> Option<Arc<RenderImage>> {
    let size = image.size(0);
    let buffer = image::ImageBuffer::from_raw(size.width.0 as u32, size.height.0 as u32, image.as_bytes(0)?.to_vec())?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Bytes of every frame of an image.
pub fn bytes_of(image: &RenderImage) -> usize {
    (0..image.frame_count()).filter_map(|frame| image.as_bytes(frame)).map(<[u8]>::len).sum()
}

/// An animation is gone: what was noted about it goes too.
pub fn forget(image: &RenderImage) {
    if let Some(plays) = PLAYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_mut() {
        plays.remove(&image.id);
    }
    RUNS.with(|runs| runs.borrow_mut().remove(&image.id));
}

/// How far into itself an animation has played.
struct Run {
    played: Duration,
    /// When it was last drawn playing; `None` while it is paused.
    last: Option<Instant>,
}

thread_local! {
    static RUNS: RefCell<HashMap<ImageId, Run>> = RefCell::new(HashMap::new());
    /// Each window's one pending wake, and when it fires.
    static WAKES: RefCell<HashMap<WindowId, (Instant, Task<()>)>> = RefCell::new(HashMap::new());
}

/// The frame to show `played` into an animation of these delays, and how long until the next
/// one; `None` once a file that plays a set number of times has finished on its last frame.
fn frame_at(delays: &[Duration], plays: Option<u32>, played: Duration) -> (usize, Option<Duration>) {
    let total: Duration = delays.iter().sum();
    if delays.len() < 2 || total.is_zero() {
        return (0, None);
    }
    if plays.is_some_and(|plays| played >= total * plays) {
        return (delays.len() - 1, None);
    }
    let mut into = Duration::from_nanos((played.as_nanos() % total.as_nanos()) as u64);
    for (frame, delay) in delays.iter().enumerate() {
        if into < *delay {
            return (frame, Some(*delay - into));
        }
        into -= *delay;
    }
    (0, Some(delays[0]))
}

/// Paints `image` into `bounds` at the frame due now, and, when it is playing, has the window
/// drawn again when the next frame is. A still image is painted as `img` would.
pub fn paint(bounds: Bounds<Pixels>, image: &Arc<RenderImage>, fit: ObjectFit, radius: Pixels, play: Play, window: &mut Window, cx: &mut App) {
    let frame = if image.frame_count() > 1 { frame_now(bounds, image, play, window, cx) } else { 0 };
    let fitted = fit.get_bounds(bounds, image.size(frame));
    let _ = window.paint_image(bounds, fitted, Corners::all(radius), image.clone(), frame, false);
}

fn frame_now(bounds: Bounds<Pixels>, image: &Arc<RenderImage>, play: Play, window: &mut Window, cx: &mut App) -> usize {
    let hovered = bounds.contains(&window.mouse_position());
    // Reduce Motion turns playing by itself into playing under the pointer.
    let by_hover = match (play, autoplay()) {
        (Play::Asked, _) | (_, Autoplay::Never) => None,
        (Play::Inline, Autoplay::Always) if !app::reduce_motion() => None,
        _ => Some(hovered),
    };
    let allowed = by_hover.unwrap_or(play == Play::Asked || autoplay() != Autoplay::Never);
    let on_screen = window.content_mask().bounds.intersects(&bounds);
    // The pointer arriving or leaving starts or stops it, so the window must hear of it.
    if on_screen && by_hover.is_some() {
        window.on_mouse_event(move |event: &MouseMoveEvent, _, window, _| {
            if bounds.contains(&event.position) != hovered {
                window.refresh();
            }
        });
    }
    let delays: Vec<Duration> = (0..image.frame_count()).map(|frame| image.delay(frame).into()).collect();
    let plays = PLAYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().and_then(|plays| plays.get(&image.id).copied());
    let now = Instant::now();
    let (frame, next) = RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        let run = runs.entry(image.id).or_insert(Run { played: Duration::ZERO, last: None });
        if !allowed {
            // Not to be played here: the first frame, from the start next time.
            *run = Run { played: Duration::ZERO, last: None };
        } else if on_screen && (window.is_window_active() || shot_frames()) {
            if let Some(last) = run.last {
                // A long gap is time it was away (scrolled off, another window); it resumes
                // where it was rather than jumping ahead.
                let longest = delays.iter().max().copied().unwrap_or_default() + Duration::from_millis(50);
                run.played += (now - last).min(longest);
            }
            run.last = Some(now);
        } else {
            run.last = None;
        }
        let (frame, next) = frame_at(&delays, plays, run.played);
        (frame, next.filter(|_| run.last.is_some()))
    });
    if let Some(next) = next {
        wake(now + next, window, cx);
    }
    frame
}

/// `--frames` in a screenshot run: play as if the window were key, which a run started from a
/// terminal is not reliably.
#[cfg(feature = "gui-shot")]
pub static SHOT_FRAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn shot_frames() -> bool {
    #[cfg(feature = "gui-shot")]
    return SHOT_FRAMES.load(Ordering::Relaxed);
    #[cfg(not(feature = "gui-shot"))]
    false
}

/// Has the window drawn at `at`, unless a wake already comes sooner.
fn wake(at: Instant, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    // A wake already past has fired (or is firing) and is replaced; a later one is dropped,
    // which cancels it: the draw this one causes schedules whatever is still due.
    let sooner = WAKES.with(|wakes| wakes.borrow().get(&id).is_none_or(|(pending, _)| at < *pending || *pending <= Instant::now()));
    if !sooner {
        return;
    }
    let task = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(at.saturating_duration_since(Instant::now())).await;
        let _ = cx.update(|window, _| window.refresh());
    });
    WAKES.with(|wakes| wakes.borrow_mut().insert(id, (at, task)));
}

/// A decoded picture filling its parent: `img` for a still, a moving one painted frame by
/// frame. The parent sets the size.
pub fn picture(image: Arc<RenderImage>, fit: ObjectFit, radius: f32, play: Play) -> AnyElement {
    if image.frame_count() < 2 {
        return img(ImageSource::Render(image)).size_full().object_fit(fit).rounded(px(radius)).into_any_element();
    }
    canvas(|_, _, _| (), move |bounds, (), window, cx| paint(bounds, &image, fit, px(radius), play, window, cx))
        .size_full()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_shown_follows_the_files_delays_and_its_play_count() {
        let ms = Duration::from_millis;
        let delays = [ms(50), ms(50), ms(100)];
        assert_eq!(frame_at(&delays, None, ms(0)), (0, Some(ms(50))));
        assert_eq!(frame_at(&delays, None, ms(60)), (1, Some(ms(40))));
        assert_eq!(frame_at(&delays, None, ms(150)), (2, Some(ms(50))));
        // Forever: round again.
        assert_eq!(frame_at(&delays, None, ms(210)), (0, Some(ms(40))));
        // Twice through, then it rests on its last frame and asks for nothing more.
        assert_eq!(frame_at(&delays, Some(2), ms(210)), (0, Some(ms(40))));
        assert_eq!(frame_at(&delays, Some(2), ms(400)), (2, None));
        assert_eq!(frame_at(&[ms(80)], None, ms(500)), (0, None));
    }

    #[test]
    fn a_budget_takes_only_what_fits_and_gets_it_back() {
        let budget = Budget { held: AtomicUsize::new(0), most: 100 };
        assert!(budget.take(60) && !budget.take(60) && budget.room(40));
        budget.give(60);
        assert!(budget.take(100) && budget.held() == 100);
        budget.give(500);
        assert_eq!(budget.held(), 0);
    }

    #[test]
    fn the_autoplay_choice_reads_back_from_its_word() {
        for autoplay in [Autoplay::Always, Autoplay::Hover, Autoplay::Never] {
            assert_eq!(Autoplay::from_word(autoplay.word()), Some(autoplay));
        }
        assert_eq!(Autoplay::from_word("sometimes"), None);
    }
}
