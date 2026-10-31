use protocol::{chat_sock_path, read_frame, write_frame, ChatStream, ErrorCode, Request, Response};
use std::io;
use std::time::Duration;

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

async fn on_thread<T: Send + 'static>(work: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    let (send, receive) = futures_channel::oneshot::channel();
    std::thread::Builder::new().name("tinyclient-ipc".into()).spawn(move || {
        let _ = send.send(work());
    })?;
    receive.await.map_err(|_| io::Error::other("the request worker stopped"))?
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
/// Logs `action failed` with `op` and [`failure_class`], never the error's display.
pub fn trouble_words(op: &'static str, err: &io::Error) -> String {
    tracing::warn!(op, class = failure_class(err), "action failed");
    if is_gone(err) {
        "tinyclient's background service isn't running. It is being started again; try again in a moment.".to_owned()
    } else if is_slow(err) {
        "That took too long. Try again.".to_owned()
    } else if is_other_version(err) {
        OTHER_ANSWER.to_owned()
    } else {
        "That didn't work. Try again.".to_owned()
    }
}

/// What a refusal from chatd says to the user, for the codes that carry no words of their own.
/// Never names chatd, a Rust value or a command (C14).
pub fn refusal_words(code: ErrorCode, detail: &str) -> String {
    match code {
        ErrorCode::TooLarge => "That was too much to load at once. Try a smaller part.".to_owned(),
        ErrorCode::Unsupported if detail.is_empty() => {
            "Parts of tinyclient are different versions. Quit and reopen tinyclient.".to_owned()
        }
        ErrorCode::Offline => "Not connected. It goes through when the network is back.".to_owned(),
        ErrorCode::Internal => "Something went wrong inside tinyclient. Try again.".to_owned(),
        _ if !detail.is_empty() => detail.to_owned(),
        _ => "That didn't work. Try again.".to_owned(),
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
            let words = refusal_words(code, "");
            assert!(!words.contains("chatd") && !words.contains("Unsupported") && words.ends_with('.'), "{words}");
        }
        assert!(refusal_words(ErrorCode::Unsupported, "").contains("different versions"));
        assert_eq!(refusal_words(ErrorCode::Refused, "Slow mode is on."), "Slow mode is on.");
        assert!(is_gone(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::TimedOut, io::ErrorKind::InvalidData, io::ErrorKind::Other] {
            let words = trouble_words("window", &io::Error::new(kind, "os error 61"));
            assert!(!words.contains("chatd") && !words.contains("os error") && words.ends_with('.'), "{words}");
        }
        assert!(is_slow(&io::Error::from(io::ErrorKind::WouldBlock)) && !is_gone(&io::Error::from(io::ErrorKind::TimedOut)));
    }

    #[test]
    fn failure_class_maps_io_kinds() {
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::TimedOut, "x")), "timeout");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::NotFound, "x")), "down");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::InvalidData, "body")), "invalid");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::PermissionDenied, "no")), "refused");
        assert_eq!(failure_class(&io::Error::new(io::ErrorKind::Other, "os error 61")), "other");
    }
}
