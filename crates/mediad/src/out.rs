//! mediad's stdout: replies and events share it, one whole frame at a time. A thread of its own
//! writes them, so a chatd that is slow to read costs the call's tick nothing.
//!
//! What cannot wait is lost first. A level or a picture is replaced by the next one, and a
//! speaking flag that does not fit is owed: the last word for that person goes out once there is
//! room, so nobody's ring stays lit. Everything else, replies and state changes above all, always
//! arrives: past [`LIMIT`] the caller waits for chatd, which only a chatd that has stopped
//! reading can cause.
//!
//! As a process (desktop) this is the real stdout and the real `exit`. In process (phones,
//! `inproc`), each mediad is an [`Instance`]: its frames go to the pipe chatd reads, its stdin is
//! the pipe chatd writes, its environment is the command's, and its `exit` ends only that mediad.
//! Which instance a thread belongs to is a thread-local, set by `inproc` on the first thread and
//! carried to every thread mediad starts through [`spawn`]. Each instance has a [`Line`] and a
//! writer of its own, which drains what is queued and then closes the pipe.

use protocol::media_host::{Closer, Exit, PipeReader};
use protocol::{write_frame, CallFailure, MediaResponse};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
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
    /// In process, this mediad ended: the writer drains what is here and goes.
    closing: bool,
    /// The writer has gone. Nothing queued now would ever arrive.
    closed: bool,
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
        !self.closed && (self.writing || !self.frames.is_empty() || !self.owed.is_empty())
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
    /// In process a thread can panic anywhere, and the app goes on: a poisoned lock is no reason
    /// for another mediad thread to fail, least of all the one saying so.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send(&self, frame: &MediaResponse) {
        let mut bytes = Vec::new();
        if write_frame(&mut bytes, frame).is_err() {
            exit(0);
        }
        if self.lock().admit(frame) {
            self.push(bytes);
        }
    }

    /// Queues bytes that must arrive. Once the writer has gone, so has chatd, and so has the call.
    fn push(&self, bytes: Vec<u8>) {
        let mut queue = self.lock();
        while queue.frames.len() >= LIMIT && !queue.closed {
            queue = self.room.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if queue.closed {
            drop(queue);
            exit(0);
        }
        queue.frames.push_back(bytes);
        self.ready.notify_one();
    }

    /// Writes until `to` fails, which it only does once chatd is gone, or until this mediad ended
    /// and all that it said is out.
    fn write(&self, mut to: impl Write) -> io::Result<()> {
        let mut queue = self.lock();
        loop {
            queue.writing = false;
            let Some(frame) = queue.next() else {
                self.idle.notify_all();
                if queue.closing {
                    return Ok(());
                }
                queue = self.ready.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            };
            queue.writing = true;
            self.room.notify_all();
            drop(queue);
            to.write_all(&frame)?;
            to.flush()?;
            queue = self.lock();
        }
    }

    /// The writer has gone: nobody waits on it any more.
    fn closed(&self) {
        self.lock().closed = true;
        self.room.notify_all();
        self.idle.notify_all();
    }

    fn flush(&self, within: Duration) {
        let queue = self.lock();
        let _ = self.idle.wait_timeout_while(queue, within, |queue| queue.busy()).unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

static LINE: OnceLock<Arc<Line>> = OnceLock::new();

/// The process's line to chatd, with its writer running.
fn process_line() -> &'static Arc<Line> {
    let mut started = false;
    let line = LINE.get_or_init(|| {
        started = true;
        Arc::default()
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

/// The line this thread's mediad talks to chatd on.
fn line() -> Arc<Line> {
    match instance() {
        Some(instance) => instance.line.clone(),
        None => process_line().clone(),
    }
}

/// One mediad running inside another process.
pub struct Instance {
    line: Arc<Line>,
    stdin: Mutex<Option<PipeReader>>,
    /// Closes stdin from anywhere, so the thread reading it sees end of file when this ends.
    input: Closer,
    env: Vec<(String, Option<String>)>,
    exit: Arc<Exit>,
}

impl Instance {
    /// With its writer running, or the thread's error: a mediad that cannot speak does not start.
    pub fn new(launch: protocol::media_host::Launch) -> io::Result<Arc<Instance>> {
        let input = launch.stdin.closer();
        let instance = Arc::new(Instance {
            line: Arc::default(),
            stdin: Mutex::new(Some(launch.stdin)),
            input,
            env: launch.env,
            exit: launch.exit,
        });
        let writer = instance.clone();
        let stdout = launch.stdout;
        // Never an instance's own thread: it outlives `finish`, for as long as frames remain, and
        // dropping `stdout` as it goes is what chatd reads as this mediad's end.
        std::thread::Builder::new().name("out".to_owned()).spawn(move || {
            if writer.line.write(stdout).is_err() {
                writer.finish(0);
            }
            writer.line.closed();
        })?;
        Ok(instance)
    }

    /// Ends this mediad with `code`: chatd sees its stdout close and reads the code, as a process
    /// exiting. What it had queued still goes first. Its threads find out through [`over`].
    pub fn finish(&self, code: i32) {
        self.exit.finish(code);
        self.line.lock().closing = true;
        self.line.ready.notify_all();
        self.input.close();
    }
}

thread_local! {
    static INSTANCE: RefCell<Option<Arc<Instance>>> = const { RefCell::new(None) };
}

/// The instance this thread belongs to, in process.
pub fn instance() -> Option<Arc<Instance>> {
    INSTANCE.with(|held| held.borrow().clone())
}

/// Makes this thread part of `instance`.
pub fn enter(instance: Option<Arc<Instance>>) {
    INSTANCE.with(|held| *held.borrow_mut() = instance);
}

/// What `exit` unwinds with, in process. Caught where each of mediad's threads starts.
pub struct Exited(pub i32);

/// Hands one frame to chatd's writer. Returns at once unless chatd has stopped reading and the
/// frame is one that must arrive.
pub fn send(frame: &MediaResponse) {
    line().send(frame);
}

/// Any frame (the player's events are not `MediaResponse`s). It must arrive.
pub fn write(frame: &impl Serialize) {
    let mut bytes = Vec::new();
    if write_frame(&mut bytes, frame).is_err() {
        exit(0);
    }
    line().push(bytes);
}

/// Waits for what is queued to reach chatd, as long as it takes up to a moment. Every way out of
/// a mediad comes through here, so the last word is not lost with it.
pub fn flush() {
    let line = match instance() {
        Some(instance) => instance.line.clone(),
        None => match LINE.get() {
            Some(line) => line.clone(),
            None => return,
        },
    };
    line.flush(FLUSH);
}

/// One line on stdout (`--devices`).
pub fn print_line(line: &str) {
    match instance() {
        None => println!("{line}"),
        Some(instance) => instance.line.push(format!("{line}\n").into_bytes()),
    }
}

/// Says why the call cannot go on, then exits: chatd shows the reason and reaps us.
pub fn fail(reason: CallFailure, detail: impl Into<String>) -> ! {
    let detail = detail.into();
    tracing::warn!("mediad: failed ({reason:?}): {detail}");
    send(&MediaResponse::Failed { reason, detail });
    exit(2);
}

/// Runs what the call lives on. Should it panic, the call is over and chatd is told: a thread
/// that just ended leaves this mediad up with no call in it, and nothing says so. An `exit` on the
/// way out, in process, is not a panic, and goes on up.
pub fn or_fail<R>(reason: CallFailure, detail: &str, work: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(done) => done,
        Err(payload) if payload.is::<Exited>() => std::panic::resume_unwind(payload),
        Err(_) => fail(reason, detail),
    }
}

/// Ends this mediad: the process, or in process this instance and the thread that called it.
/// What is queued for chatd goes first.
pub fn exit(code: i32) -> ! {
    flush();
    match instance() {
        None => std::process::exit(code),
        Some(instance) => {
            instance.finish(code);
            std::panic::resume_unwind(Box::new(Exited(code)))
        }
    }
}

/// True once this mediad has ended (in process: finished or killed). Always false as a process,
/// whose end takes every thread with it.
pub fn over() -> bool {
    instance().is_some_and(|instance| instance.exit.ended())
}

/// chatd's frames to us: stdin, or this instance's pipe. In process it may be taken once.
pub fn stdin() -> Box<dyn Read + Send> {
    match instance() {
        None => Box::new(io::stdin()),
        Some(instance) => match instance.stdin.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            Some(pipe) => Box::new(pipe),
            None => Box::new(io::empty()),
        },
    }
}

/// An environment variable, as the command that started this mediad set it.
pub fn var_os(name: &str) -> Option<OsString> {
    if let Some(instance) = instance() {
        if let Some((_, value)) = instance.env.iter().rev().find(|(known, _)| known == name) {
            return value.clone().map(OsString::from);
        }
    }
    std::env::var_os(name)
}

/// [`var_os`] as a string, like `std::env::var`.
pub fn var(name: &str) -> Option<String> {
    var_os(name).and_then(|value| value.into_string().ok())
}

/// Starts one of mediad's threads. As a process this is `builder.spawn(f)`. In process the
/// thread joins this thread's instance, and a panic or an `exit` on it ends the instance, not
/// the app: a crash in a call takes down the call. The thread then ends as a panicked thread
/// does (`join` is `Err`), without running the panic hook a second time.
pub fn spawn<F, T>(builder: std::thread::Builder, f: F) -> io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let Some(instance) = instance() else {
        return builder.spawn(f);
    };
    builder.spawn(move || {
        enter(Some(instance.clone()));
        match guarded(&instance, f) {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// Runs `f` for `instance`, turning an `exit` or a panic into the instance's end.
pub fn guarded<T>(instance: &Arc<Instance>, f: impl FnOnce() -> T) -> Result<T, Box<dyn std::any::Any + Send>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).inspect_err(|payload| {
        if !payload.is::<Exited>() {
            tracing::warn!("mediad: a thread panicked; this mediad ends and the app goes on");
            instance.finish(101);
        }
    })
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

    /// `fail` ends the process, so the panic happens in a copy of this test run on its own.
    #[test]
    fn a_panic_in_the_call_fails_it_in_words() {
        const ALONE: &str = "MEDIAD_TEST_OR_FAIL";
        if std::env::var_os(ALONE).is_some() {
            or_fail(CallFailure::Engine, "the call stopped", || panic!("the tick"));
        }
        let test = "out::tests::a_panic_in_the_call_fails_it_in_words";
        let alone = std::process::Command::new(std::env::current_exe().unwrap()).args([test, "--exact"]).env(ALONE, "1").output().unwrap();
        assert_eq!(alone.status.code(), Some(2), "{alone:?}");
        assert!(String::from_utf8_lossy(&alone.stdout).contains("the call stopped"), "{alone:?}");
    }

    /// One mediad in this process, as `inproc` runs it, doing `$body` on its first thread.
    macro_rules! in_process {
        ($body:expr) => {{
            fn start(launch: protocol::media_host::Launch) -> io::Result<()> {
                let instance = Instance::new(launch)?;
                std::thread::spawn(move || {
                    enter(Some(instance.clone()));
                    let code = guarded(&instance, $body).map_or(101, |()| 0);
                    instance.finish(code);
                    enter(None);
                });
                Ok(())
            }
            let spawned = protocol::media_host::spawn_with(start, &std::process::Command::new("mediad")).unwrap();
            let mut bytes = Vec::new();
            spawned.stdout.unwrap().read_to_end(&mut bytes).unwrap();
            let mut child = spawned.child;
            let code = child.wait().unwrap();
            let mut frames: Vec<MediaResponse> = Vec::new();
            let mut left = &bytes[..];
            while let Ok(frame) = read_frame(&mut left) {
                frames.push(frame);
            }
            (frames, code.code)
        }};
    }

    #[test]
    fn in_process_a_panic_in_the_call_fails_that_mediad_and_the_app_goes_on() {
        let (frames, code) = in_process!(|| or_fail(CallFailure::Engine, "the call stopped", || panic!("the tick")));
        assert!(matches!(&frames[..], [MediaResponse::Failed { detail, .. }] if detail == "the call stopped"), "{frames:?}");
        assert_eq!(code, Some(2));
    }

    #[test]
    fn in_process_an_exit_inside_the_call_is_not_a_panic() {
        let (frames, code) = in_process!(|| or_fail(CallFailure::Engine, "the call stopped", || exit(7)));
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!(code, Some(7));
    }

    #[test]
    fn in_process_everything_said_before_the_end_arrives() {
        let (frames, code) = in_process!(|| {
            for n in 0..2000 {
                send(&MediaResponse::Notice { detail: n.to_string() });
            }
        });
        assert_eq!(frames.len(), 2000);
        assert_eq!(frames.last(), Some(&MediaResponse::Notice { detail: "1999".to_owned() }));
        assert_eq!(code, Some(0));
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
