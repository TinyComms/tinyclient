//! mediad's stdout: replies and events share it, one whole frame at a time.

use protocol::{write_frame, CallFailure, MediaResponse};
use std::io;
use std::sync::Mutex;

static OUT: Mutex<()> = Mutex::new(());

/// Writes one frame to chatd. A closed pipe means chatd is gone, and so is the call.
pub fn send(frame: &MediaResponse) {
    let _held = OUT.lock().unwrap();
    if write_frame(io::stdout().lock(), frame).is_err() {
        std::process::exit(0);
    }
}

/// Says why the call cannot go on, then exits: chatd shows the reason and reaps us.
pub fn fail(reason: CallFailure, detail: impl Into<String>) -> ! {
    let detail = detail.into();
    tracing::warn!("mediad: failed ({reason:?}): {detail}");
    send(&MediaResponse::Failed { reason, detail });
    std::process::exit(2);
}
