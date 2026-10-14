use protocol::{chat_sock_path, read_frame, write_frame, ChatStream, Request, Response};
use std::io;

/// One request to chatd over its local socket.
pub fn ask(request: &Request) -> io::Result<Response> {
    let mut stream = ChatStream::connect(&chat_sock_path())?;
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
}
