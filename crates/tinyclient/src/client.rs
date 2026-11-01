use protocol::{chat_sock_path, read_frame, write_frame, ChatStream, ErrorCode, Request, Response};
use std::io;
use std::time::Duration;

/// Writing a request never takes longer than this: chatd reads every frame at once.
const WRITE: Duration = Duration::from_secs(5);
/// An answer chatd owes at once. `Wait` and every network call answer within 30 s on chatd's
/// side, so this is only reached by a chatd that has stopped answering.
const READ: Duration = Duration::from_secs(45);

/// One request to chatd over its local socket. A sign-in waits on the user, so it has no read
/// timeout; everything else gives up after [`READ`] with `TimedOut` / `WouldBlock`.
pub fn ask(request: &Request) -> io::Result<Response> {
    let read = match request {
        Request::SignIn { .. } => None,
        _ => Some(READ),
    };
    ask_within(request, read)
}

/// [`ask`] with its own read timeout: `Status` at launch is asked within 3 s, so a chatd stuck
/// behind a keychain prompt is told apart from one that is not there.
pub fn ask_within(request: &Request, read: Option<Duration>) -> io::Result<Response> {
    // `--shot --demo`: a made-up chatd answers, and the real one is never reached.
    #[cfg(feature = "gui-shot")]
    if crate::gui::shot_demo::on() {
        return crate::gui::shot_demo::answer(request);
    }
    let mut stream = ChatStream::connect(&chat_sock_path())?;
    stream.set_read_timeout(read)?;
    let _ = stream.set_write_timeout(Some(WRITE));
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
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

/// What a failed request says to the user, in place of the system's error text (C14).
pub fn trouble_words(err: &io::Error) -> String {
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
    fn refusals_read_as_words_and_never_name_the_service() {
        for code in [ErrorCode::TooLarge, ErrorCode::Unsupported, ErrorCode::Internal, ErrorCode::NotFound] {
            let words = refusal_words(code, "");
            assert!(!words.contains("chatd") && !words.contains("Unsupported") && words.ends_with('.'), "{words}");
        }
        assert!(refusal_words(ErrorCode::Unsupported, "").contains("different versions"));
        assert_eq!(refusal_words(ErrorCode::Refused, "Slow mode is on."), "Slow mode is on.");
        assert!(is_gone(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::TimedOut, io::ErrorKind::InvalidData, io::ErrorKind::Other] {
            let words = trouble_words(&io::Error::new(kind, "os error 61"));
            assert!(!words.contains("chatd") && !words.contains("os error") && words.ends_with('.'), "{words}");
        }
        assert!(is_slow(&io::Error::from(io::ErrorKind::WouldBlock)) && !is_gone(&io::Error::from(io::ErrorKind::TimedOut)));
    }
}
