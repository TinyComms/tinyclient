//! The shared agent keeps a connection for the next request, and `call` asks again on a new one
//! when a kept connection was closed under it. Only a read, and a message post that carries a
//! `client_id` (the server answers it once), are asked twice.

use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    requests: Mutex<Vec<String>>,
}

/// A keep-alive HTTP server. The nth connection answers `script[n]` requests; with `drop_next`
/// it then reads one more request and closes without an answer, which is what a connection a
/// middlebox cut in the middle looks like to the client. A connection past the script answers
/// without end.
fn serve(script: &[usize], drop_next: bool) -> (String, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Arc::new(Seen::default());
    let (script, shared) = (script.to_vec(), seen.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let nth = shared.connections.fetch_add(1, Ordering::SeqCst);
            let answers = script.get(nth).copied().unwrap_or(usize::MAX);
            let drop_next = drop_next && answers != usize::MAX;
            let shared = shared.clone();
            std::thread::spawn(move || answer(stream, answers, drop_next, &shared));
        }
    });
    (base, seen)
}

/// Reads one request and records its first line. None when the connection ended first.
fn read_request(reader: &mut BufReader<std::net::TcpStream>, seen: &Seen) -> Option<()> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let request = line.trim_end().to_owned();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    seen.requests.lock().unwrap().push(request);
    Some(())
}

fn answer(stream: std::net::TcpStream, answers: usize, drop_next: bool, seen: &Seen) {
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);
    for _ in 0..answers {
        if read_request(&mut reader, seen).is_none() {
            return;
        }
        let json = r#"{"ok":true}"#;
        let reply = format!(
            "HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: {}

{json}",
            json.len()
        );
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
    if drop_next {
        // Taken, never answered, and closed.
        let _ = read_request(&mut reader, seen);
    }
}

fn connections(seen: &Seen) -> usize {
    seen.connections.load(Ordering::SeqCst)
}

fn requests(seen: &Seen) -> Vec<String> {
    seen.requests.lock().unwrap().clone()
}

#[test]
fn a_second_request_reuses_the_kept_connection() {
    let (base, seen) = serve(&[2], false);
    for _ in 0..2 {
        assert!(matches!(call("GET", &format!("{base}/a"), None, None), Ok((200, _))));
    }
    assert_eq!(connections(&seen), 1, "the second request rode the first connection");
    assert_eq!(requests(&seen).len(), 2);
}

#[test]
fn a_read_on_a_connection_that_dropped_it_is_asked_again() {
    let (base, seen) = serve(&[1], true);
    assert!(matches!(call("GET", &format!("{base}/a"), None, None), Ok((200, _))));
    assert!(matches!(call("GET", &format!("{base}/b"), None, None), Ok((200, _))));
    assert_eq!(connections(&seen), 2, "the second read was asked again on a new connection");
    let seen = requests(&seen);
    assert_eq!(seen.iter().filter(|line| line.starts_with("GET /b")).count(), 2, "{seen:?}");
}

#[test]
fn a_post_with_a_client_id_on_a_dropped_connection_is_asked_again() {
    let (base, seen) = serve(&[1], true);
    assert!(matches!(call("GET", &format!("{base}/a"), None, None), Ok((200, _))));
    let body = json!({"body": "hi", "client_id": "c1"});
    assert!(matches!(call("POST", &format!("{base}/m"), None, Some(body)), Ok((200, _))));
    assert_eq!(connections(&seen), 2);
    let seen = requests(&seen);
    assert_eq!(seen.iter().filter(|line| line.starts_with("POST /m")).count(), 2, "{seen:?}");
}

#[test]
fn a_post_without_a_client_id_is_never_asked_twice() {
    let (base, seen) = serve(&[1], true);
    assert!(matches!(call("GET", &format!("{base}/a"), None, None), Ok((200, _))));
    let body = json!({"body": "hi"});
    let answer = call("POST", &format!("{base}/m"), None, Some(body));
    assert!(matches!(answer, Err(Fault::Unreachable)), "a dropped post is an error, not a second post");
    assert_eq!(connections(&seen), 1, "no new connection was made for it");
    let seen = requests(&seen);
    assert_eq!(seen.iter().filter(|line| line.starts_with("POST /m")).count(), 1, "{seen:?}");
}
