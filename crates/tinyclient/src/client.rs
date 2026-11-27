use protocol::{chat_sock_path, read_frame, write_frame, ChatStream, ErrorCode, Request, Response};
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Maximum concurrent OS worker threads serving async IPC calls.
const MAX_IPC_WORKERS: usize = 32;
/// Maximum queued async IPC jobs waiting for a worker before failing fast with `WouldBlock`.
const MAX_IPC_QUEUED: usize = 128;
/// Idle IPC workers exit after this long without a job.
const IPC_WORKER_IDLE: Duration = Duration::from_secs(30);

/// Writing a request never takes longer than this: chatd reads every frame at once.
const WRITE: Duration = Duration::from_secs(5);
/// An answer chatd owes at once. `Wait` and every network call answer within 30 s on chatd's
/// side, so this is only reached by a chatd that has stopped answering.
const READ: Duration = Duration::from_secs(45);

/// One request to chatd over its local socket. A sign-in waits on the user, so it has no read
/// timeout. Tiny authentication allows 90 s for sequential server requests; other
/// requests give up after [`READ`] with `TimedOut` / `WouldBlock`.
pub fn ask(request: &Request) -> io::Result<Response> {
    let read = match request {
        Request::SignIn { .. } => None,
        // Tiny sign-in may fetch configuration and start authorization in sequence.
        Request::SignInAnswer { network: protocol::Network::Tiny, .. }
        | Request::SignInCancel { network: protocol::Network::Tiny } => Some(Duration::from_secs(90)),
        _ => Some(READ),
    };
    ask_within(request, read)
}

/// [`ask`] with its own read timeout: `Status` at launch is asked within 3 s, so a chatd stuck
/// behind a keychain prompt is told apart from one that is not there.
pub fn ask_within(request: &Request, read: Option<Duration>) -> io::Result<Response> {
    // `--shot --demo`, or a `demo` phone build: a made-up chatd answers, and the real one is
    // never reached.
    #[cfg(any(feature = "gui-shot", feature = "demo"))]
    if crate::gui::shot_demo::on() {
        return crate::gui::shot_demo::answer(request);
    }
    let mut stream = ChatStream::connect(&chat_sock_path())?;
    stream.set_read_timeout(read)?;
    let _ = stream.set_write_timeout(Some(WRITE));
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
}

/// A blocking socket wait must not occupy GPUI's limited background worker pool.
/// In particular, two long polls can otherwise prevent sign-in from even beginning.
pub async fn ask_async(request: Request) -> io::Result<Response> {
    on_thread(move || ask(&request)).await
}

pub async fn ask_within_async(request: Request, read: Option<Duration>) -> io::Result<Response> {
    on_thread(move || ask_within(&request, read)).await
}

type IpcJob = Box<dyn FnOnce() + Send + 'static>;

struct PoolState {
    queue: VecDeque<IpcJob>,
    workers: usize,
    idle: usize,
}

struct IpcPool {
    state: Mutex<PoolState>,
    wake: Condvar,
    max_workers: usize,
    max_queued: usize,
    idle_timeout: Duration,
}

impl IpcPool {
    fn new(max_workers: usize, max_queued: usize, idle_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PoolState { queue: VecDeque::new(), workers: 0, idle: 0 }),
            wake: Condvar::new(),
            max_workers: max_workers.max(1),
            max_queued: max_queued.max(1),
            idle_timeout,
        })
    }

    fn shared() -> &'static Arc<Self> {
        static POOL: OnceLock<Arc<IpcPool>> = OnceLock::new();
        POOL.get_or_init(|| Self::new(MAX_IPC_WORKERS, MAX_IPC_QUEUED, IPC_WORKER_IDLE))
    }

    fn submit(self: &Arc<Self>, job: IpcJob) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.queue.len() >= self.max_queued {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "too many in-flight IPC requests"));
        }
        state.queue.push_back(job);
        let need_worker = state.idle == 0 && state.workers < self.max_workers;
        if need_worker {
            state.workers += 1;
            let pool = Arc::clone(self);
            drop(state);
            match std::thread::Builder::new().name("tinyclient-ipc".into()).spawn(move || pool.run_worker()) {
                Ok(_) => Ok(()),
                Err(err) => {
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    state.workers = state.workers.saturating_sub(1);
                    if state.workers == 0 {
                        let _ = state.queue.pop_back();
                        return Err(err);
                    }
                    self.wake.notify_one();
                    Ok(())
                }
            }
        } else {
            self.wake.notify_one();
            Ok(())
        }
    }

    fn run_worker(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(job) = state.queue.pop_front() {
                drop(state);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            state.idle += 1;
            let (next, waited) = self
                .wake
                .wait_timeout(state, self.idle_timeout)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
            state.idle = state.idle.saturating_sub(1);
            if waited.timed_out() && state.queue.is_empty() {
                state.workers = state.workers.saturating_sub(1);
                break;
            }
        }
    }

    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let (send, receive) = futures_channel::oneshot::channel();
        self.submit(Box::new(move || {
            let _ = send.send(work());
        }))?;
        receive.await.map_err(|_| io::Error::other("the request worker stopped"))?
    }
}

async fn on_thread<T: Send + 'static>(work: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    IpcPool::shared().run(work).await
}

/// The socket is not there or nobody holds it: chatd is not running and may be started.
pub fn is_gone(err: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(err.kind(), NotFound | ConnectionRefused | ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof)
}

/// chatd holds the socket but did not answer in time.
pub fn is_slow(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

/// An answer that could not be read: chatd and the shell are different versions.
pub fn is_other_version(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::InvalidData
}

/// What the window says when chatd answered with something the shell did not ask for: only a
/// chatd of another version does that.
pub const OTHER_ANSWER: &str = "Parts of tinyclient are different versions. Quit and reopen tinyclient.";

/// `down`, `timeout`, `refused`, `invalid`, or `other`. The error's own text is not included.
pub fn failure_class(err: &io::Error) -> &'static str {
    match err.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => "down",
        io::ErrorKind::TimedOut => "timeout",
        io::ErrorKind::PermissionDenied => "refused",
        io::ErrorKind::InvalidData => "invalid",
        _ => "other",
    }
}

/// What a failed request says to the user, in place of the system's error text (C14).
/// A fallback names `op` and a short ref. The log line with that ref has the real error.
pub fn trouble_words(op: &'static str, err: &io::Error) -> String {
    if is_gone(err) {
        tracing::warn!(op, class = failure_class(err), "action failed");
        "tinyclient's background service isn't running. It is being started again; try again in a moment.".to_owned()
    } else if is_slow(err) {
        tracing::warn!(op, class = failure_class(err), "action failed");
        "That took too long. Try again.".to_owned()
    } else if is_other_version(err) {
        tracing::warn!(op, class = failure_class(err), "action failed");
        OTHER_ANSWER.to_owned()
    } else {
        with_ref(op, failure_class(err), &err.to_string(), &format!("Couldn't {}. Try again.", action_phrase(op)))
    }
}

/// What a refusal from chatd says to the user, for the codes that carry no words of their own.
/// `action` is required, so a fallback can name what was tried. Never names chatd, a Rust
/// value or a command (C14). A server's own words are shown as sent, unless they are the
/// bare fallback.
pub fn refusal_words(action: &'static str, code: ErrorCode, detail: &str) -> String {
    match code {
        ErrorCode::TooLarge => "That was too much to load at once. Try a smaller part.".to_owned(),
        ErrorCode::Unsupported if detail.is_empty() => {
            "Parts of tinyclient are different versions. Quit and reopen tinyclient.".to_owned()
        }
        ErrorCode::Offline if !detail.is_empty() => detail.to_owned(),
        ErrorCode::Offline => "Not connected.".to_owned(),
        ErrorCode::Internal => with_ref(
            action,
            "internal",
            "internal",
            &format!("Couldn't {}. Something went wrong inside tinyclient. Try again.", action_phrase(action)),
        ),
        _ if !detail.is_empty() && !is_bare(detail) => detail.to_owned(),
        _ => with_ref(action, "refused", code_name(code), &format!("Couldn't {}. Try again.", action_phrase(action))),
    }
}

/// The lab's error-text driver. A build without it opens the window and stays up.
pub fn prove_error_text() -> Result<(), &'static str> {
    let words = trouble_words("send", &io::Error::other("blocked"));
    if words.contains("That didn't work") || !words.contains("Couldn't send that") || !words.contains("(ref ") {
        return Err("a failure still has no action");
    }
    let refused = refusal_words("pin that message", ErrorCode::NotFound, "");
    if !refused.starts_with("Couldn't pin that message") || !refused.contains("(ref ") || refused.contains("That didn't work") {
        return Err("a refusal still has no action");
    }
    let bare = refusal_words("pin that message", ErrorCode::Refused, "That didn't work. Try again.");
    if !bare.starts_with("Couldn't pin that message") || bare.contains("That didn't work") {
        return Err("the bare sentence is still shown");
    }
    if !fallback_notes().contains("send") {
        return Err("diagnostics omit the ref");
    }
    Ok(())
}

/// Recent fallbacks for Copy diagnostics: the ref, the action, and how many times.
/// The real error stays in the log.
pub fn fallback_notes() -> String {
    let rows = fallbacks().lock().unwrap_or_else(|err| err.into_inner());
    if rows.is_empty() {
        return String::new();
    }
    let mut text = String::from("recent failures:\n");
    for row in rows.iter() {
        text.push_str(&format!("ref {} {} ({})\n", row.quote, row.action, row.hits));
    }
    text
}

struct Fallback {
    quote: String,
    action: &'static str,
    hits: u32,
}

fn fallbacks() -> &'static std::sync::Mutex<Vec<Fallback>> {
    static ROWS: std::sync::OnceLock<std::sync::Mutex<Vec<Fallback>>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Shows `sentence` and appends `(ref XXXX)`. Logs `cause` with the same ref, and counts the hit.
fn with_ref(action: &'static str, class: &str, cause: &str, sentence: &str) -> String {
    let quote = quote_code(action, class);
    let hits = note_fallback(action, &quote);
    let cause = one_line(cause);
    tracing::warn!(action, quote = %quote, hits, cause = %cause, "fallback");
    format!("{sentence} (ref {quote}).")
}

fn note_fallback(action: &'static str, quote: &str) -> u32 {
    let mut rows = fallbacks().lock().unwrap_or_else(|err| err.into_inner());
    if let Some(row) = rows.iter_mut().find(|row| row.quote == quote) {
        row.hits = row.hits.saturating_add(1);
        return row.hits;
    }
    if rows.len() >= 8 {
        rows.remove(0);
    }
    rows.push(Fallback { quote: quote.to_owned(), action, hits: 1 });
    1
}

fn quote_code(action: &str, class: &str) -> String {
    let mut hash = 0x811c_9dc5u32;
    for byte in action.bytes().chain(class.bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:04X}", (hash >> 16) as u16)
}

fn one_line(cause: &str) -> String {
    cause.chars().map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch }).take(180).collect()
}

fn is_bare(detail: &str) -> bool {
    matches!(
        detail.trim(),
        "That didn't work. Try again." | "That didn't work. Try again" | "That didn't work." | "That didn't work"
            | "That didn't work. Check it and try again." | "That didn't work. Check it and try again"
    )
}

fn action_phrase(action: &str) -> &str {
    match action {
        "send" => "send that",
        "inbox" => "refresh the list",
        "tiny" => "do that on this server",
        "load" => "open that conversation",
        "people" => "load people",
        "call" => "change the call",
        "page" => "load that page",
        "save" => "save that",
        "play" => "play that",
        "search" => "search",
        "sign-in" => "sign in",
        other => other,
    }
}

fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Offline => "offline",
        ErrorCode::NotFound => "not found",
        ErrorCode::CallActive => "call active",
        ErrorCode::NoCall => "no call",
        ErrorCode::Media => "media",
        ErrorCode::SignIn => "sign in",
        ErrorCode::SignInUnreachable => "sign in unreachable",
        ErrorCode::SignInBlocked => "sign in blocked",
        ErrorCode::Unsupported => "unsupported",
        ErrorCode::Refused => "refused",
        ErrorCode::TooLarge => "too large",
        ErrorCode::Internal => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_request_does_not_block_other_async_requests() {
        let (release, waiting) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        let (waited, answered) = futures_lite::future::block_on(futures_lite::future::zip(
            on_thread(move || {
                assert_ne!(std::thread::current().id(), caller);
                waiting.recv_timeout(Duration::from_secs(2)).map_err(io::Error::other)
            }),
            async move {
                let answer = on_thread(|| Ok(42)).await?;
                release.send(()).map_err(io::Error::other)?;
                Ok::<_, io::Error>(answer)
            },
        ));
        waited.unwrap();
        assert_eq!(answered.unwrap(), 42);
    }

    #[test]
    fn refusals_read_as_words_and_never_name_the_service() {
        for code in [ErrorCode::TooLarge, ErrorCode::Unsupported, ErrorCode::Internal, ErrorCode::NotFound] {
            let words = refusal_words("pin that message", code, "");
            assert!(!words.contains("chatd") && !words.contains("Unsupported") && words.ends_with('.'), "{words}");
        }
        assert!(refusal_words("pin that message", ErrorCode::Unsupported, "").contains("different versions"));
        assert_eq!(refusal_words("pin that message", ErrorCode::Refused, "Slow mode is on."), "Slow mode is on.");
        assert_eq!(refusal_words("pin that message", ErrorCode::Offline, ""), "Not connected.");
        assert_eq!(
            refusal_words("pin that message", ErrorCode::Offline, "Discord is offline. Files need a connection."),
            "Discord is offline. Files need a connection."
        );
        assert!(is_gone(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::TimedOut, io::ErrorKind::InvalidData, io::ErrorKind::Other] {
            let words = trouble_words("window", &io::Error::new(kind, "os error 61"));
            assert!(!words.contains("chatd") && !words.contains("os error") && words.ends_with('.'), "{words}");
        }
        assert!(is_slow(&io::Error::from(io::ErrorKind::WouldBlock)) && !is_gone(&io::Error::from(io::ErrorKind::TimedOut)));
    }

    #[test]
    fn a_fallback_names_the_action_and_a_ref() {
        let err = io::Error::other("os error 5");
        let words = trouble_words("send", &err);
        assert!(words.contains("Couldn't send that"), "{words}");
        assert!(words.contains("(ref "), "{words}");
        assert!(!words.contains("That didn't work"), "{words}");
        assert!(!words.contains("os error"), "{words}");
        let refused = refusal_words("pin that message", ErrorCode::NotFound, "");
        assert!(refused.starts_with("Couldn't pin that message"), "{refused}");
        assert!(refused.contains("(ref "), "{refused}");
        assert!(!refused.contains("That didn't work"), "{refused}");
        let bare = refusal_words("pin that message", ErrorCode::Refused, "That didn't work. Try again.");
        assert!(bare.starts_with("Couldn't pin that message"), "{bare}");
        assert!(!bare.contains("That didn't work"), "{bare}");
        let notes = fallback_notes();
        assert!(notes.contains("send"), "{notes}");
        assert!(notes.contains("pin that message"), "{notes}");
    }

    #[test]
    fn failure_class_maps_io_kinds() {
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::TimedOut, "x")), "timeout");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::NotFound, "x")), "down");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::InvalidData, "body")), "invalid");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::PermissionDenied, "no")), "refused");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::Other, "os error 61")), "other");
    }

    #[test]
    fn ipc_pool_bounds_workers_and_rejects_excess_backlog() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pool = IpcPool::new(2, 2, Duration::from_secs(1));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let entered_tx = entered_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            pool.submit(Box::new(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let _ = entered_tx.send(());
                let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(2));
                active.fetch_sub(1, Ordering::SeqCst);
                let _ = done_tx.send(());
            }))
            .expect("worker slot available");
            handles.push(done_rx);
        }

        // Wait for both workers to be occupied.
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Fill the bounded queue (2 slots) while both workers are busy.
        for _ in 0..2 {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            pool.submit(Box::new(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
                let _ = done_tx.send(());
            }))
            .expect("queue slot available");
            handles.push(done_rx);
        }

        // A 5th submission exceeds max_queued and fails fast with WouldBlock.
        let overflow = pool.submit(Box::new(|| {})).unwrap_err();
        assert_eq!(overflow.kind(), io::ErrorKind::WouldBlock);

        // Release both blocked workers; all 4 accepted jobs complete and peak concurrency never exceeded 2.
        let _ = release_tx.send(());
        let _ = release_tx.send(());
        for done_rx in handles {
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }
}
