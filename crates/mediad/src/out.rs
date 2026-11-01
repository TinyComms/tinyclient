//! mediad's stdout: replies and events share it, one whole frame at a time. A thread of its own
//! writes them, so a chatd that is slow to read costs the call's tick nothing.
//!
//! What cannot wait is lost first. A level or a picture is replaced by the next one, and a
//! speaking flag that does not fit is owed: the last word for that person goes out once there is
//! room, so nobody's ring stays lit. Everything else, replies and state changes above all, always
//! arrives: past [`LIMIT`] the caller waits for chatd, which only a chatd that has stopped
//! reading can cause.

use protocol::{write_frame, CallFailure, MediaResponse};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

/// Frames waiting before what may be lost is let go.
const ROOM: usize = 128;
/// Frames waiting before a frame that must arrive waits for room.
const LIMIT: usize = 1024;
/// How long [`flush`] waits for a chatd that does not read. chatd reaps us at two seconds.
const FLUSH: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Queue {
    frames: VecDeque<Vec<u8>>,
    /// The last word on who is speaking, for each person whose flag did not fit.
    owed: HashMap<String, bool>,
    /// The writer holds a frame it took from `frames`.
    writing: bool,
}

impl Queue {
    /// Whether `frame` goes in the queue. False when it is let go; a speaking flag is then owed.
    fn admit(&mut self, frame: &MediaResponse) -> bool {
        let full = self.frames.len() >= ROOM;
        match frame {
            MediaResponse::Speaking { user_id, speaking } if full => {
                self.owed.insert(user_id.clone(), *speaking);
                false
            }
            MediaResponse::Speaking { user_id, .. } => {
                self.owed.remove(user_id);
                true
            }
            MediaResponse::Levels { .. } => !full,
            MediaResponse::Picture { path, .. } if !path.is_empty() => !full,
            _ => true,
        }
    }

    /// The next frame to write. What is owed goes once the queue has drained.
    fn next(&mut self) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            for (user_id, speaking) in self.owed.drain() {
                let mut bytes = Vec::new();
                if write_frame(&mut bytes, &MediaResponse::Speaking { user_id, speaking }).is_ok() {
                    self.frames.push_back(bytes);
                }
            }
        }
        self.frames.pop_front()
    }

    fn busy(&self) -> bool {
        self.writing || !self.frames.is_empty() || !self.owed.is_empty()
    }
}

#[derive(Default)]
struct Line {
    queue: Mutex<Queue>,
    /// There is something to write.
    ready: Condvar,
    /// There is room in the queue.
    room: Condvar,
    /// The queue drained.
    idle: Condvar,
}

impl Line {
    fn send(&self, frame: &MediaResponse) {
        let mut bytes = Vec::new();
        if write_frame(&mut bytes, frame).is_err() {
            std::process::exit(0);
        }
        let mut queue = self.queue.lock().unwrap();
        if !queue.admit(frame) {
            return;
        }
        while queue.frames.len() >= LIMIT {
            queue = self.room.wait(queue).unwrap();
        }
        queue.frames.push_back(bytes);
        self.ready.notify_one();
    }

    /// Writes until `to` fails. It only fails once chatd is gone.
    fn write(&self, mut to: impl Write) -> io::Result<()> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            queue.writing = false;
            let Some(frame) = queue.next() else {
                self.idle.notify_all();
                queue = self.ready.wait(queue).unwrap();
                continue;
            };
            queue.writing = true;
            self.room.notify_all();
            drop(queue);
            to.write_all(&frame)?;
            to.flush()?;
            queue = self.queue.lock().unwrap();
        }
    }

    fn flush(&self, within: Duration) {
        let queue = self.queue.lock().unwrap();
        let _ = self.idle.wait_timeout_while(queue, within, |queue| queue.busy()).unwrap();
    }
}

static LINE: OnceLock<Line> = OnceLock::new();

/// The line to chatd, with its writer running.
fn line() -> &'static Line {
    let mut started = false;
    let line = LINE.get_or_init(|| {
        started = true;
        Line::default()
    });
    if started {
        // A closed pipe means chatd is gone, and so is the call. A mediad that cannot start this
        // can say nothing, which chatd sees as the process ending.
        std::thread::Builder::new()
            .name("out".to_owned())
            .spawn(|| {
                let line = LINE.get().expect("set before the writer starts");
                let _ = line.write(io::stdout());
                std::process::exit(0);
            })
            .expect("the thread that writes to chatd");
    }
    line
}

/// Hands one frame to chatd's writer. Returns at once unless chatd has stopped reading and the
/// frame is one that must arrive.
pub fn send(frame: &MediaResponse) {
    line().send(frame);
}

/// Waits for what is queued to reach chatd, as long as it takes up to a moment. Every way out of
/// the process comes through here, so the last word is not lost with it.
pub fn flush() {
    if let Some(line) = LINE.get() {
        line.flush(FLUSH);
    }
}

/// Says why the call cannot go on, then exits: chatd shows the reason and reaps us.
pub fn fail(reason: CallFailure, detail: impl Into<String>) -> ! {
    let detail = detail.into();
    tracing::warn!("mediad: failed ({reason:?}): {detail}");
    send(&MediaResponse::Failed { reason, detail });
    flush();
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::read_frame;
    use std::sync::mpsc::{sync_channel, Receiver};
    use std::sync::Arc;

    /// A chatd that takes one frame each time the test says so.
    struct Stalled {
        turn: Receiver<()>,
        seen: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.turn.recv().map_err(|_| io::Error::other("the test is over"))?;
            self.seen.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn stalled() -> (Arc<Line>, std::sync::mpsc::SyncSender<()>, Arc<Mutex<Vec<u8>>>) {
        let line = Arc::new(Line::default());
        let (go, turn) = sync_channel(0);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (writer, sink) = (line.clone(), Stalled { turn, seen: seen.clone() });
        std::thread::spawn(move || writer.write(sink));
        (line, go, seen)
    }

    fn written(seen: &Mutex<Vec<u8>>) -> Vec<MediaResponse> {
        let bytes = seen.lock().unwrap().clone();
        let mut from = &bytes[..];
        let mut frames = Vec::new();
        while !from.is_empty() {
            frames.push(read_frame(&mut from).unwrap());
        }
        frames
    }

    fn speaking(user_id: &str, speaking: bool) -> MediaResponse {
        MediaResponse::Speaking { user_id: user_id.into(), speaking }
    }

    #[test]
    fn a_full_queue_drops_levels_and_owes_the_last_word_on_who_speaks() {
        let (line, go, seen) = stalled();
        for n in 0..ROOM * 3 {
            line.send(&MediaResponse::Levels { input: (n % 100) as u8, output: 0 });
        }
        line.send(&speaking("ada", true));
        line.send(&speaking("ada", false));
        line.send(&MediaResponse::Interrupted);
        assert!(line.queue.lock().unwrap().frames.len() <= ROOM + 2, "the sender did not wait and the queue stayed bounded");
        while line.queue.lock().unwrap().busy() {
            let _ = go.try_send(());
            std::thread::yield_now();
        }
        let frames = written(&seen);
        assert!(frames.contains(&MediaResponse::Interrupted), "a state change is never let go");
        assert_eq!(frames.last(), Some(&speaking("ada", false)), "the ring goes out after the queue drains");
        assert!(!frames.contains(&speaking("ada", true)), "only the last word is kept");
    }

    #[test]
    fn a_frame_that_fits_cancels_what_was_owed_for_that_person() {
        let mut queue = Queue::default();
        queue.frames.extend((0..ROOM).map(|_| Vec::new()));
        assert!(!queue.admit(&speaking("ada", true)));
        assert_eq!(queue.owed.get("ada"), Some(&true));
        queue.frames.clear();
        assert!(queue.admit(&speaking("ada", false)));
        assert!(queue.owed.is_empty());
    }

    #[test]
    fn replies_and_state_are_never_let_go_and_an_empty_picture_is_state() {
        let mut queue = Queue::default();
        queue.frames.extend((0..ROOM).map(|_| Vec::new()));
        let picture = |path: &str| MediaResponse::Picture { path: path.into(), width: 1, height: 1, generation: 1, user_id: String::new(), kind: Default::default() };
        assert!(!queue.admit(&picture("/tmp/a.jpg")));
        assert!(queue.admit(&picture("")));
        for frame in [MediaResponse::Done, MediaResponse::Interrupted, MediaResponse::Sending { share: true, camera: false }] {
            assert!(queue.admit(&frame), "{frame:?}");
        }
    }

    #[test]
    fn a_flush_waits_for_the_queue_and_gives_up_on_a_chatd_that_never_reads() {
        let (line, go, seen) = stalled();
        line.send(&MediaResponse::Done);
        line.flush(Duration::from_millis(20));
        assert!(written(&seen).is_empty(), "the flush gave up on a stalled chatd");
        let takes = std::thread::spawn(move || while go.send(()).is_ok() {});
        line.flush(Duration::from_secs(5));
        assert_eq!(written(&seen), [MediaResponse::Done]);
        drop(line);
        drop(takes);
    }
}
