//! Headless sender scenario: the real daemon, its IPC and the Tiny adapter, against a loopback
//! server with invented people. The person sends a message and the pending row must already
//! name them, and must settle as soon as the post is answered, even while the server is slow to
//! list its members. This does not certify native window behavior.

use protocol::{
    read_frame, write_frame, AccountKey, ChatStream, Key, Network, Outgoing, Page, Request, Response, SendState,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ME: &str = "11111111-1111-1111-1111-111111111111";
const PEER: &str = "22222222-2222-2222-2222-222222222222";
const DM: &str = "33333333-3333-3333-3333-333333333333";
const WHEN: &str = "2024-01-02T03:04:05Z";
const EMAIL: &str = "ada@example.test";
const TEXT: &str = "on my way";
/// How long the invented server takes to list its members once the person is connected.
const SLOW_MEMBERS: Duration = Duration::from_millis(3000);
/// How long a send may take to settle. Well under [`SLOW_MEMBERS`]: a send that waits for the
/// members list cannot meet it.
const SETTLES_WITHIN: Duration = Duration::from_millis(1500);

pub(super) fn run() -> Result<(), String> {
    // Never read a real session or open the installed profile, even if called outside the driver.
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    std::env::set_var("TINYCLIENT_DIR", dir.path());
    std::env::set_var("TINYCLIENT_NOTIFY", "off");
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!("http://{}", listener.local_addr().map_err(|err| err.to_string())?);
    let slow = Arc::new(AtomicBool::new(false));
    let slow_here = slow.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let slow = slow_here.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut peek = [0; 16];
                if stream.peek(&mut peek).is_err() {
                    return;
                }
                if peek.starts_with(b"GET /v1/live") {
                    let Ok(mut ws) = tungstenite::accept(stream) else { return };
                    let ready = json!({"type":"ready", "seq":1, "me":{"user_id":ME,"role":"member"}, "online":[ME,PEER], "calls":[]});
                    if ws.send(tungstenite::Message::text(ready.to_string())).is_err() {
                        return;
                    }
                    // Hold the socket open; nothing else is said.
                    std::thread::sleep(Duration::from_secs(60));
                    return;
                }
                let mut request = Vec::new();
                let mut buf = [0; 2048];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let (status, body) = if request.starts_with("GET /v1/members ") {
                    if slow.load(Ordering::Relaxed) {
                        std::thread::sleep(SLOW_MEMBERS);
                    }
                    (
                        200,
                        json!([
                        {"user_id":ME,"display_name":"Ada","email":EMAIL,"role":"member","created_at":WHEN},
                        {"user_id":PEER,"display_name":"Bea","email":"bea@example.test","role":"member","created_at":WHEN}]),
                    )
                } else if request.starts_with("GET /v1/me ") {
                    (200, json!({"user_id":ME,"display_name":"Ada","email":EMAIL,"role":"member"}))
                } else if request.starts_with("GET /v1/directs ") {
                    (
                        200,
                        json!({"directs":[{"id":DM,"peer":{"user_id":PEER,"display_name":"Bea"},"unread":0}]}),
                    )
                } else if request.starts_with("GET /v1/channels ") {
                    (200, json!({"channels":[]}))
                } else if request.starts_with("GET /v1/rooms ") {
                    (200, json!([]))
                } else if request.starts_with(&format!("GET /v1/conversations/{DM}/messages?")) {
                    (200, json!({"messages":[]}))
                } else if request.starts_with(&format!("POST /v1/conversations/{DM}/messages ")) {
                    // The client_id the person's app made is read back out of the body.
                    let client_id = body_of(&mut stream, &request)
                        .and_then(|body| body.get("client_id").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default();
                    (
                        201,
                        json!({"id":"44444444-4444-4444-4444-444444444444","author_id":ME,"body":TEXT,"client_id":client_id,"created_at":WHEN}),
                    )
                } else {
                    (404, json!({}))
                };
                let body = body.to_string();
                let answer = format!(
                    "HTTP/1.1 {status} -\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    for network in Network::ALL {
        std::env::remove_var(format!("TINYCLIENT_SESSION_{}", network.label().to_uppercase()));
    }
    std::env::set_var(
        format!("TINYCLIENT_SESSION_{}", Network::Tiny.label().to_uppercase()),
        json!({"base_url":base,"token":"invented-local-token","user_id":ME,
        "display_name":"Ada","email":EMAIL,"role":"member","plain_http_ok":false})
        .to_string(),
    );
    crate::spawn_core(dir.path().to_owned(), crate::Host::InProcess, |_| Ok(())).map_err(|err| err.to_string())?;
    let socket = protocol::chat_sock_path_in(dir.path());
    let ask = |request: Request| -> Result<Response, String> {
        let mut stream = ChatStream::connect(&socket).map_err(|err| err.to_string())?;
        stream.set_read_timeout(Some(Duration::from_secs(8))).map_err(|err| err.to_string())?;
        write_frame(&mut stream, &request).map_err(|err| err.to_string())?;
        read_frame(&mut stream).map_err(|err| err.to_string())
    };
    let account = AccountKey { network: Network::Tiny, account: ME.to_owned() };
    let conversation = Key::new(&account, DM);
    let until = |what: &str, wait: Duration, check: &dyn Fn() -> Result<bool, String>| -> Result<(), String> {
        let deadline = Instant::now() + wait;
        loop {
            if check()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("{what} did not happen"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let eight = Duration::from_secs(8);
    until("Tiny connected and told who we are", eight, &|| {
        Ok(matches!(ask(Request::Status)?, Response::Status { accounts, .. }
            if accounts.iter().any(|own| own.key == account && own.link == protocol::Link::Connected && !own.me.is_empty())))
    })?;
    // Who we are on the account is our person id. The email is not an id anyone is looked up by.
    let Response::Status { accounts, .. } = ask(Request::Status)? else { return Err("no status".into()) };
    let own = accounts.iter().find(|own| own.key == account).ok_or("no account row")?;
    if own.me != ME {
        return Err(format!("the account's own id was {:?}, not the person's id", own.me));
    }
    until("the conversation listed", eight, &|| {
        Ok(matches!(ask(Request::Conversation { conversation: conversation.clone() })?, Response::Conversation { .. }))
    })?;
    eprintln!("send-prove: connected, own id is the person id");

    // From here the server is slow to list members.
    slow.store(true, Ordering::Relaxed);
    let client_id = "55555555-5555-4555-8555-555555555555".to_owned();
    let began = Instant::now();
    let sent = ask(Request::Send {
        conversation: conversation.clone(),
        client_id: client_id.clone(),
        text: TEXT.to_owned(),
        reply_to: None,
        with: Outgoing::default(),
    })?;
    let Response::Sent { message } = sent else { return Err(format!("send was answered {sent:?}")) };
    if message.sender.id != ME || message.sender.name != "Ada" || message.shown_sender() != "Ada" {
        return Err(format!(
            "the pending row names {:?} (id {:?}), not the person",
            message.shown_sender(),
            message.sender.id
        ));
    }
    eprintln!("send-prove: the pending row names the person");
    until("the pending row settled", SETTLES_WITHIN, &|| {
        let Response::Messages { messages, .. } =
            ask(Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 10 })?
        else {
            return Ok(false);
        };
        Ok(messages.iter().any(|row| row.client_id.as_deref() == Some(client_id.as_str()) && row.state == SendState::Sent))
    })
    .map_err(|why| format!("{why} within {} ms", SETTLES_WITHIN.as_millis()))?;
    eprintln!("send-prove: settled after {} ms", began.elapsed().as_millis());
    Ok(())
}

/// The JSON body of the request, read past the head the caller already has.
fn body_of(stream: &mut std::net::TcpStream, head: &str) -> Option<Value> {
    let (head, already) = head.split_once("\r\n\r\n")?;
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
        })?;
    let mut body = already.as_bytes().to_vec();
    let mut buf = [0; 2048];
    while body.len() < length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    serde_json::from_slice(&body).ok()
}
