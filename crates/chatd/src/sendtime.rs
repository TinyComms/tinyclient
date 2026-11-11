//! Where the time of one send goes.
//!
//! A send is cheap to the person who presses Enter and slow to the one who waits for the row to
//! settle, and nothing used to say which part was slow. [`submitted`] marks the moment chatd
//! took the request; the adapter that carries it out wraps the work in [`run`], and each stage of
//! that work calls [`stage`]. Every stage logs at `info` with its `elapsed_ms`, the whole send
//! logs once when it ends, and a send over [`SLOW_MS`] adds one `warn` with the breakdown.
//!
//! Only the opaque `client_id` is logged. Never the text, a name, an address or a person's id.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A send that takes longer than this is logged as a `warn` with its stages.
pub const SLOW_MS: u64 = 1000;
/// A submit nobody picked up (a network that does not time sends) is forgotten after this.
const FORGET_AFTER: Duration = Duration::from_secs(300);
/// The most submits remembered at once.
const MOST: usize = 256;

fn submits() -> &'static Mutex<HashMap<String, Instant>> {
    static SUBMITS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    SUBMITS.get_or_init(Default::default)
}

/// The request for `client_id` was taken. Call it for a network whose adapter calls [`run`].
pub fn submitted(client_id: &str) {
    let mut seen = submits().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if seen.len() >= MOST {
        seen.retain(|_, at| at.elapsed() < FORGET_AFTER);
        if seen.len() >= MOST {
            return;
        }
    }
    seen.insert(client_id.to_owned(), Instant::now());
}

/// The stages of one send, in order, each with the time it took.
#[derive(Debug)]
pub struct Stopwatch {
    client_id: String,
    origin: Instant,
    last: Instant,
    stages: Vec<(&'static str, u64)>,
}

impl Stopwatch {
    fn new(client_id: &str) -> Self {
        let now = Instant::now();
        let origin = submits().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(client_id).unwrap_or(now);
        let mut watch = Stopwatch { client_id: client_id.to_owned(), origin, last: origin, stages: Vec::new() };
        watch.mark("queue");
        watch
    }

    fn mark(&mut self, name: &'static str) {
        let now = Instant::now();
        let took = millis(now.duration_since(self.last));
        self.last = now;
        self.stages.push((name, took));
        tracing::info!(client_id = %self.client_id, stage = name, elapsed_ms = took, "chatd: send stage");
    }

    fn finish(self, ok: bool) {
        let total = millis(self.origin.elapsed());
        let outcome = if ok { "sent" } else { "failed" };
        tracing::info!(client_id = %self.client_id, outcome, elapsed_ms = total, "chatd: send finished");
        if total > SLOW_MS {
            tracing::warn!(
                client_id = %self.client_id,
                outcome,
                elapsed_ms = total,
                stages = %breakdown(&self.stages),
                "chatd: slow send"
            );
        }
    }
}

/// `queue=3 seal=410 post=220`, for the slow-send warning.
pub fn breakdown(stages: &[(&'static str, u64)]) -> String {
    stages.iter().map(|(name, took)| format!("{name}={took}")).collect::<Vec<_>>().join(" ")
}

fn millis(took: Duration) -> u64 {
    took.as_millis().min(u128::from(u64::MAX)) as u64
}

thread_local! {
    static CURRENT: RefCell<Option<Stopwatch>> = const { RefCell::new(None) };
}

/// Runs one send on this thread, timed from its submit. `work` returns whether it succeeded.
pub fn run<T, E>(client_id: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let previous = CURRENT.with(|current| current.borrow_mut().replace(Stopwatch::new(client_id)));
    let result = work();
    let watch = CURRENT.with(|current| std::mem::replace(&mut *current.borrow_mut(), previous));
    if let Some(watch) = watch {
        watch.finish(result.is_ok());
    }
    result
}

/// The named stage of the send running on this thread just ended. Nothing when none is running.
pub fn stage(name: &'static str) {
    CURRENT.with(|current| {
        if let Some(watch) = current.borrow_mut().as_mut() {
            watch.mark(name);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_records_its_stages_in_order_and_no_stage_outside_one() {
        stage("ignored");
        let mut seen = Vec::new();
        let result: Result<u8, ()> = run("client-1", || {
            stage("seal");
            stage("post");
            CURRENT.with(|current| {
                seen = current.borrow().as_ref().map(|watch| watch.stages.iter().map(|(name, _)| *name).collect()).unwrap_or_default();
            });
            Ok(1)
        });
        assert_eq!(result, Ok(1));
        assert_eq!(seen, ["queue", "seal", "post"]);
        assert!(CURRENT.with(|current| current.borrow().is_none()));
    }

    #[test]
    fn the_breakdown_names_each_stage_and_never_anything_else() {
        assert_eq!(breakdown(&[("queue", 3), ("post", 1200)]), "queue=3 post=1200");
    }

    #[test]
    fn a_submit_is_taken_once() {
        submitted("client-2");
        assert!(submits().lock().unwrap().contains_key("client-2"));
        let _ = Stopwatch::new("client-2");
        assert!(!submits().lock().unwrap().contains_key("client-2"));
    }
}
