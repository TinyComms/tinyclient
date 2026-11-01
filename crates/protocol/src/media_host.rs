//! Where mediad runs: its own process (desktop), or threads inside this one (phones).
//!
//! chatd and the shell start mediad for a call, a voice message (`--play`), a notification sound
//! (`--sound`), Settings' test (`test`) and the device list (`--devices`). On a desktop each is a
//! child process that talks length-prefixed frames over its stdin and stdout. A phone cannot run a
//! helper binary, so its mobile entry registers an in-process starter ([`register`]); [`spawn`]
//! then hands the same arguments and environment to that starter and connects it with an
//! in-memory byte pipe ([`pipe`]) instead of stdin and stdout. The frames on the pipe are the
//! same bytes, so neither side reads them differently.
//!
//! Without a registered starter [`spawn`] is exactly `command.stdin(piped).stdout(piped).spawn()`.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Everything an in-process mediad gets in place of a process: what `main` would have read from
/// its arguments and environment, its stdin and stdout, and where it reports its exit status.
pub struct Launch {
    /// The arguments after the program name, as the command had them.
    pub args: Vec<String>,
    /// Variables the command set (`Some`) or removed (`None`). The rest of this process's
    /// environment applies as it is.
    pub env: Vec<(String, Option<String>)>,
    /// chatd's frames to mediad.
    pub stdin: PipeReader,
    /// mediad's frames to chatd. Dropping it is mediad closing its stdout.
    pub stdout: PipeWriter,
    /// Set once, when this mediad is over.
    pub exit: Arc<Exit>,
}

/// Starts one mediad on threads of this process and returns at once. It must report its end
/// through `launch.exit` (and drop `launch.stdout`), whatever way it ends.
pub type MediaStarter = fn(Launch) -> io::Result<()>;

static STARTER: OnceLock<MediaStarter> = OnceLock::new();

/// Registers the in-process starter. Only the first registration counts.
pub fn register(starter: MediaStarter) {
    let _ = STARTER.set(starter);
}

/// True when mediad runs inside this process.
pub fn in_process() -> bool {
    STARTER.get().is_some()
}

/// A running mediad, as chatd holds it.
pub struct Spawned {
    pub child: MediaChild,
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
}

/// Starts mediad as `command` names it: the process itself on a desktop, or the registered
/// in-process starter, given the command's arguments and environment, on a phone.
pub fn spawn(command: &mut Command) -> io::Result<Spawned> {
    if let Some(starter) = STARTER.get() {
        return spawn_with(*starter, command);
    }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    let stdin = child.stdin.take().map(|pipe| Box::new(pipe) as Box<dyn Write + Send>);
    let stdout = child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
    Ok(Spawned { child: MediaChild { inner: Inner::Process(child) }, stdin, stdout })
}

/// [`spawn`] through a given starter, registered or not. Tests use it.
pub fn spawn_with(starter: MediaStarter, command: &Command) -> io::Result<Spawned> {
    let text = |value: &OsStr| value.to_string_lossy().into_owned();
    let (to_media, stdin) = pipe();
    let (stdout, from_media) = pipe();
    let exit = Exit::new();
    let launch = Launch {
        args: command.get_args().map(text).collect(),
        env: command.get_envs().map(|(name, value)| (text(name), value.map(text))).collect(),
        stdin,
        stdout,
        exit: exit.clone(),
    };
    starter(launch)?;
    Ok(Spawned {
        child: MediaChild { inner: Inner::InProcess { exit, input: to_media.closer() } },
        stdin: Some(Box::new(to_media)),
        stdout: Some(Box::new(from_media)),
    })
}

/// The whole of what mediad printed on stdout, given nothing on stdin: `--devices`. The
/// process's `output()` on a desktop.
pub fn output(command: &mut Command) -> io::Result<Vec<u8>> {
    if STARTER.get().is_none() {
        return command.output().map(|output| output.stdout);
    }
    let mut spawned = spawn(command)?;
    spawned.stdin.take();
    let mut bytes = Vec::new();
    if let Some(mut stdout) = spawned.stdout.take() {
        stdout.read_to_end(&mut bytes)?;
    }
    let _ = spawned.child.wait();
    Ok(bytes)
}

/// How a mediad ended. `code` is `None` when it was killed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub code: Option<i32>,
}

impl Status {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The child half of [`Spawned`]: a process, or an in-process mediad.
pub struct MediaChild {
    inner: Inner,
}

enum Inner {
    Process(std::process::Child),
    InProcess { exit: Arc<Exit>, input: Closer },
}

impl From<std::process::Child> for MediaChild {
    /// A mediad process started some other way (with its own stdio).
    fn from(child: std::process::Child) -> MediaChild {
        MediaChild { inner: Inner::Process(child) }
    }
}

impl MediaChild {
    /// Its status if it has ended, without waiting.
    pub fn try_wait(&mut self) -> io::Result<Option<Status>> {
        match &mut self.inner {
            Inner::Process(child) => Ok(child.try_wait()?.map(|status| Status { code: status.code() })),
            Inner::InProcess { exit, .. } => Ok(exit.status()),
        }
    }

    /// Waits for it to end.
    pub fn wait(&mut self) -> io::Result<Status> {
        match &mut self.inner {
            Inner::Process(child) => Ok(Status { code: child.wait()?.code() }),
            Inner::InProcess { exit, .. } => Ok(exit.wait(None).unwrap_or(Status { code: None })),
        }
    }

    /// Ends it now. In process there is nothing to kill: its input is closed, which a mediad takes
    /// as the hang-up, and it is told it was killed (`Exit::killed`) so its loops stop. It counts as
    /// ended from here; any thread still unwinding finishes on its own.
    pub fn kill(&mut self) -> io::Result<()> {
        match &mut self.inner {
            Inner::Process(child) => child.kill(),
            Inner::InProcess { exit, input } => {
                input.close();
                exit.kill();
                Ok(())
            }
        }
    }
}

/// An in-process mediad's end: set once, read by chatd as a process's exit status.
pub struct Exit {
    state: Mutex<ExitState>,
    ended: Condvar,
}

#[derive(Default)]
struct ExitState {
    status: Option<Status>,
    killed: bool,
}

impl Exit {
    pub fn new() -> Arc<Exit> {
        Arc::new(Exit { state: Mutex::new(ExitState::default()), ended: Condvar::new() })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ExitState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records the exit code. The first call wins, as a process exits once.
    pub fn finish(&self, code: i32) {
        let mut state = self.lock();
        if state.status.is_none() {
            state.status = Some(Status { code: Some(code) });
        }
        self.ended.notify_all();
    }

    fn kill(&self) {
        let mut state = self.lock();
        state.killed = true;
        if state.status.is_none() {
            state.status = Some(Status { code: None });
        }
        self.ended.notify_all();
    }

    /// True once it has ended, by itself or killed: its threads should stop.
    pub fn ended(&self) -> bool {
        self.lock().status.is_some()
    }

    /// True when chatd killed it.
    pub fn killed(&self) -> bool {
        self.lock().killed
    }

    pub fn status(&self) -> Option<Status> {
        self.lock().status
    }

    /// Waits until it ended, for `limit` at most (`None`: as long as it takes).
    pub fn wait(&self, limit: Option<Duration>) -> Option<Status> {
        let deadline = limit.map(|limit| Instant::now() + limit);
        let mut state = self.lock();
        while state.status.is_none() {
            match deadline {
                None => state = self.ended.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner()),
                Some(deadline) => {
                    let left = deadline.checked_duration_since(Instant::now())?;
                    state = self.ended.wait_timeout(state, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
                }
            }
        }
        state.status
    }
}

// ---- the in-memory pipe ----

/// The most bytes a pipe holds before a writer waits. A frame is at most 1 MiB, and a reader is
/// always draining, so this only bounds a reader that stopped.
const PIPE_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct PipeState {
    bytes: VecDeque<u8>,
    /// The writer is gone: a reader reads what is left, then end of file.
    writer_gone: bool,
    /// The reader is gone: a write fails as on a broken pipe.
    reader_gone: bool,
}

struct Shared {
    state: Mutex<PipeState>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, PipeState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One direction of bytes between two threads, with a process pipe's ends: end of file when the
/// writer is dropped, a broken pipe when the reader is.
pub fn pipe() -> (PipeWriter, PipeReader) {
    let shared = Arc::new(Shared { state: Mutex::new(PipeState::default()), changed: Condvar::new() });
    (PipeWriter { shared: shared.clone() }, PipeReader { shared })
}

pub struct PipeWriter {
    shared: Arc<Shared>,
}

pub struct PipeReader {
    shared: Arc<Shared>,
}

/// Closes a pipe from anywhere, as if its writer were dropped: a blocked reader returns end of
/// file and later writes fail.
#[derive(Clone)]
pub struct Closer {
    shared: Arc<Shared>,
}

impl Closer {
    pub fn close(&self) {
        let mut state = self.shared.lock();
        state.writer_gone = true;
        state.reader_gone = true;
        self.shared.changed.notify_all();
    }
}

impl PipeWriter {
    pub fn closer(&self) -> Closer {
        Closer { shared: self.shared.clone() }
    }
}

impl PipeReader {
    pub fn closer(&self) -> Closer {
        Closer { shared: self.shared.clone() }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.shared.lock();
        loop {
            if state.reader_gone || state.writer_gone {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the other end is gone"));
            }
            if state.bytes.len() < PIPE_LIMIT || buf.is_empty() {
                break;
            }
            state = self.shared.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let room = PIPE_LIMIT.saturating_sub(state.bytes.len()).max(1).min(buf.len());
        state.bytes.extend(&buf[..room]);
        self.shared.changed.notify_all();
        Ok(room)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.writer_gone = true;
        self.shared.changed.notify_all();
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.lock();
        loop {
            if !state.bytes.is_empty() {
                let n = buf.len().min(state.bytes.len());
                for (slot, byte) in buf.iter_mut().zip(state.bytes.drain(..n)) {
                    *slot = byte;
                }
                self.shared.changed.notify_all();
                return Ok(n);
            }
            if state.writer_gone {
                return Ok(0);
            }
            state = self.shared.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.reader_gone = true;
        self.shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cross_the_pipe_and_a_dropped_writer_is_end_of_file() {
        let (mut writer, mut reader) = pipe();
        crate::write_frame(&mut writer, &crate::Response::Pong).unwrap();
        assert_eq!(crate::read_frame::<crate::Response>(&mut reader).unwrap(), crate::Response::Pong);
        drop(writer);
        let err = crate::read_frame::<crate::Response>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_dropped_reader_breaks_the_pipe_and_a_closer_wakes_a_blocked_reader() {
        let (mut writer, reader) = pipe();
        drop(reader);
        assert_eq!(writer.write(b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);

        let (writer, mut reader) = pipe();
        let closer = reader.closer();
        let blocked = std::thread::spawn(move || {
            let mut buf = [0u8; 4];
            reader.read(&mut buf).unwrap()
        });
        std::thread::sleep(Duration::from_millis(50));
        closer.close();
        assert_eq!(blocked.join().unwrap(), 0);
        drop(writer);
    }

    fn echo(launch: Launch) -> io::Result<()> {
        std::thread::spawn(move || {
            let Launch { args, env, mut stdin, mut stdout, exit } = launch;
            assert_eq!(args, ["1"]);
            assert!(env.contains(&("TINYCLIENT_X".to_owned(), Some("y".to_owned()))));
            while let Ok(frame) = crate::read_frame::<crate::Response>(&mut stdin) {
                crate::write_frame(&mut stdout, &frame).unwrap();
            }
            drop(stdout);
            exit.finish(0);
        });
        Ok(())
    }

    #[test]
    fn an_in_process_media_child_reads_like_a_process() {
        let mut command = Command::new("mediad");
        command.arg("1").env("TINYCLIENT_X", "y");
        let mut spawned = spawn_with(echo, &command).unwrap();
        let (mut stdin, mut stdout) = (spawned.stdin.take().unwrap(), spawned.stdout.take().unwrap());
        crate::write_frame(&mut stdin, &crate::Response::Pong).unwrap();
        assert_eq!(crate::read_frame::<crate::Response>(&mut stdout).unwrap(), crate::Response::Pong);
        assert_eq!(spawned.child.try_wait().unwrap(), None);
        drop(stdin);
        assert!(spawned.child.wait().unwrap().success());
        assert_eq!(crate::read_frame::<crate::Response>(&mut stdout).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn killing_an_in_process_child_ends_it_at_once() {
        fn stuck(launch: Launch) -> io::Result<()> {
            std::thread::spawn(move || {
                let mut stdin = launch.stdin;
                let mut buf = Vec::new();
                let _ = stdin.read_to_end(&mut buf);
                launch.exit.finish(0);
            });
            Ok(())
        }
        let mut spawned = spawn_with(stuck, &Command::new("mediad")).unwrap();
        spawned.child.kill().unwrap();
        assert_eq!(spawned.child.wait().unwrap(), Status { code: None });
    }
}
