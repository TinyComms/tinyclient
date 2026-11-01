//! mediad's stdout: replies and events share it, one whole frame at a time.
//!
//! As a process (desktop) this is the real stdout and the real `exit`. In process (phones,
//! `inproc`), each mediad is an [`Instance`]: its frames go to the pipe chatd reads, its stdin is
//! the pipe chatd writes, its environment is the command's, and its `exit` ends only that mediad.
//! Which instance a thread belongs to is a thread-local, set by `inproc` on the first thread and
//! carried to every thread mediad starts through [`spawn`].

use protocol::media_host::{Closer, Exit, PipeReader, PipeWriter};
use protocol::{write_frame, CallFailure, MediaResponse};
use serde::Serialize;
use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

static OUT: Mutex<()> = Mutex::new(());

/// One mediad running inside another process.
pub struct Instance {
    stdout: Mutex<Option<PipeWriter>>,
    stdin: Mutex<Option<PipeReader>>,
    /// Closes stdin from anywhere, so the thread reading it sees end of file when this ends.
    input: Closer,
    env: Vec<(String, Option<String>)>,
    exit: Arc<Exit>,
}

impl Instance {
    pub fn new(launch: protocol::media_host::Launch) -> Arc<Instance> {
        let input = launch.stdin.closer();
        Arc::new(Instance {
            stdout: Mutex::new(Some(launch.stdout)),
            stdin: Mutex::new(Some(launch.stdin)),
            input,
            env: launch.env,
            exit: launch.exit,
        })
    }

    /// Ends this mediad with `code`: chatd sees its stdout close and reads the code, as a process
    /// exiting. Its threads find out through [`over`].
    pub fn finish(&self, code: i32) {
        self.exit.finish(code);
        self.stdout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
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

/// Writes one frame to chatd. A closed pipe means chatd is gone, and so is the call.
pub fn send(frame: &MediaResponse) {
    write(frame);
}

/// Writes any frame (the player's events are not `MediaResponse`s).
pub fn write(frame: &impl Serialize) {
    let Some(instance) = instance() else {
        let held = OUT.lock().unwrap();
        if write_frame(io::stdout().lock(), frame).is_err() {
            drop(held);
            std::process::exit(0);
        }
        return;
    };
    let written = {
        let mut stdout = instance.stdout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match stdout.as_mut() {
            Some(pipe) => write_frame(pipe, frame).is_ok(),
            None => false,
        }
    };
    if !written {
        exit(0);
    }
}

/// One line on stdout (`--devices`).
pub fn print_line(line: &str) {
    match instance() {
        None => println!("{line}"),
        Some(instance) => {
            let mut stdout = instance.stdout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(pipe) = stdout.as_mut() {
                let _ = writeln!(pipe, "{line}");
            }
        }
    }
}

/// Says why the call cannot go on, then exits: chatd shows the reason and reaps us.
pub fn fail(reason: CallFailure, detail: impl Into<String>) -> ! {
    let detail = detail.into();
    tracing::warn!("mediad: failed ({reason:?}): {detail}");
    send(&MediaResponse::Failed { reason, detail });
    exit(2);
}

/// Ends this mediad: the process, or in process this instance and the thread that called it.
pub fn exit(code: i32) -> ! {
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
