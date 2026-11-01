//! The in-process core (`start_in_process`, what a phone runs) on this host: it serves the socket
//! the shell reads, answers the same requests as the chatd binary, and refuses to exit the process.

use protocol::{chat_sock_path_in, read_frame, write_frame, ChatStream, ErrorCode, Request, Response};

#[test]
fn the_in_process_core_answers_on_its_socket_and_refuses_to_exit_the_app() {
    let dir = std::env::temp_dir().join(format!("chatd-in-process-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // A scratch directory: the core reads no keychain and signs nobody in.
    std::env::set_var("TINYCLIENT_DIR", &dir);

    let core = chatd::start_in_process(dir.clone()).expect("the core starts");
    let mut stream = ChatStream::connect(&chat_sock_path_in(&dir)).expect("the socket answers once start returns");

    write_frame(&mut stream, &Request::Ping).unwrap();
    assert_eq!(read_frame::<Response>(&mut stream).unwrap(), Response::Pong);

    write_frame(&mut stream, &Request::Status).unwrap();
    let status = read_frame::<Response>(&mut stream).unwrap();
    assert!(matches!(status, Response::Status { .. }), "{status:?}");

    // The process is the app's: a Shutdown is refused with a reason, not answered Ok.
    write_frame(&mut stream, &Request::Shutdown).unwrap();
    match read_frame::<Response>(&mut stream).unwrap() {
        Response::Error { code: ErrorCode::Unsupported, .. } => {}
        other => panic!("Shutdown in process must be refused, got {other:?}"),
    }

    // One core per directory: a second start finds the first one's lock.
    let err = chatd::start_in_process(dir.clone()).err().expect("a second core is refused");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");

    // Still answering after the refused Shutdown and the refused second start.
    write_frame(&mut stream, &Request::Ping).unwrap();
    assert_eq!(read_frame::<Response>(&mut stream).unwrap(), Response::Pong);
    drop(core);
}
