//! Headless recipient scenario: real daemon IPC, live WebSocket and history command, with
//! invented people on a loopback server. This does not certify native window behavior.

use protocol::{
    read_frame, write_frame, AccountKey, ChatStream, InboxFilter, Key, Network, Page, Request,
    Response,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const ME: &str = "11111111-1111-1111-1111-111111111111";
const PEER: &str = "22222222-2222-2222-2222-222222222222";
const DM: &str = "33333333-3333-3333-3333-333333333333";
const WHEN: &str = "2024-01-02T03:04:05Z";

pub(super) fn run() -> Result<(), String> {
    // Never read a real session or open the installed profile, even if called outside the driver.
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    std::env::set_var("TINYCLIENT_DIR", dir.path());
    std::env::set_var("TINYCLIENT_NOTIFY", "off");
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|err| err.to_string())?
    );
    let (emit, events) = mpsc::channel::<Value>();
    let events = Arc::new(Mutex::new(events));
    let (history, histories) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (events, history) = (events.clone(), history.clone());
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut peek = [0; 16];
                if stream.peek(&mut peek).is_err() {
                    return;
                }
                if peek.starts_with(b"GET /v1/live") {
                    let Ok(mut ws) = tungstenite::accept(stream) else {
                        return;
                    };
                    let ready = json!({"type":"ready", "seq":1, "me":{"user_id":ME,"role":"member"}, "online":[ME,PEER], "calls":[]});
                    if ws
                        .send(tungstenite::Message::text(ready.to_string()))
                        .is_err()
                    {
                        return;
                    }
                    while let Ok(event) =
                        events.lock().unwrap().recv_timeout(Duration::from_secs(15))
                    {
                        if ws
                            .send(tungstenite::Message::text(event.to_string()))
                            .is_err()
                        {
                            return;
                        }
                    }
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
                let request = String::from_utf8_lossy(&request);
                let (status, body) = if request.starts_with("GET /v1/members ") {
                    (
                        200,
                        json!([
                        {"user_id":ME,"display_name":"Ada","email":"ada@example.test","role":"member","created_at":WHEN},
                        {"user_id":PEER,"display_name":"Bea","email":"bea@example.test","role":"member","created_at":WHEN}]),
                    )
                } else if request.starts_with("GET /v1/me ") {
                    (
                        200,
                        json!({"user_id":ME,"display_name":"Ada","email":"ada@example.test","role":"member"}),
                    )
                } else if request.starts_with("GET /v1/channels ")
                    || request.starts_with("GET /v1/directs ")
                    || request.starts_with("GET /v1/rooms ")
                {
                    // Always empty: a REST refresh cannot hide a broken live-event decoder.
                    (200, json!([]))
                } else if request.starts_with(&format!("GET /v1/conversations/{DM}/messages?")) {
                    let _ = history.send(());
                    (
                        200,
                        json!({"messages":[{"id":"older","author_id":PEER,"body":"history after opening","created_at":"2024-01-01T03:04:05Z"}]}),
                    )
                } else {
                    (404, json!({}))
                };
                let body = body.to_string();
                let answer = format!("HTTP/1.1 {status} -\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    for network in Network::ALL {
        std::env::remove_var(format!(
            "TINYCLIENT_SESSION_{}",
            network.label().to_uppercase()
        ));
    }
    std::env::set_var(
        format!(
            "TINYCLIENT_SESSION_{}",
            Network::Tiny.label().to_uppercase()
        ),
        json!({"base_url":base,"token":"invented-local-token","user_id":ME,
        "display_name":"Ada","role":"member","plain_http_ok":false})
        .to_string(),
    );
    crate::spawn_core(dir.path().to_owned(), crate::Host::InProcess, |_| Ok(()))
        .map_err(|err| err.to_string())?;
    let socket = protocol::chat_sock_path_in(dir.path());
    let ask = |request: Request| -> Result<Response, String> {
        let mut stream = ChatStream::connect(&socket).map_err(|err| err.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|err| err.to_string())?;
        write_frame(&mut stream, &request).map_err(|err| err.to_string())?;
        read_frame(&mut stream).map_err(|err| err.to_string())
    };
    let account = AccountKey {
        network: Network::Tiny,
        account: ME.to_owned(),
    };
    let conversation = Key::new(&account, DM);
    let until = |what: &str, check: &dyn Fn() -> Result<bool, String>| -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if check()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("{what} did not happen"));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    };
    // ready must finish publishing its initially empty tree before creation is emitted.
    until("Tiny connected", &|| {
        Ok(
            matches!(ask(Request::Status)?, Response::Status { accounts, .. }
        if accounts.iter().any(|own| own.key == account && own.link == protocol::Link::Connected)),
        )
    })?;
    let created = json!({"type":"direct.created","seq":2,"direct":{"id":DM,"low_user_id":ME,"high_user_id":PEER,"created_at":WHEN,"e2ee":false}});
    emit.send(created.clone()).map_err(|err| err.to_string())?;
    until("named direct published before first message", &|| {
        Ok(matches!(
        ask(Request::Conversation { conversation: conversation.clone() })?, Response::Conversation { conversation: row }
        if row.title == "Bea" && row.peer.as_deref() == Some(PEER) && row.kind == protocol::ConversationKind::Direct))
    })?;
    eprintln!("direct-created: named peer published");
    emit.send(json!({"type":"message.created","seq":3,"conversation_id":DM,"message":{"id":"first","author_id":PEER,"body":"hello Ada","created_at":WHEN}})).map_err(|err| err.to_string())?;
    until("first message in named direct inbox", &|| {
        Ok(matches!(
        ask(Request::Inbox { network:Some(Network::Tiny),filter:InboxFilter::Directs,offset:0,limit:10 })?,
        Response::Conversations { conversations, .. } if conversations.len() == 1 && conversations[0].title == "Bea"
            && conversations[0].preview == "hello Ada" && conversations[0].unread == 1))
    })?;
    eprintln!("direct-created: first message in inbox with unread and preview");
    // A duplicate creation with a new event sequence must not clear message state.
    let mut duplicate = created;
    duplicate["seq"] = json!(4);
    emit.send(duplicate).map_err(|err| err.to_string())?;
    // A later marker proves that the socket consumed the duplicate, rather than an early read passing.
    emit.send(json!({"type":"message.created","seq":5,"conversation_id":DM,"message":{"id":"second","author_id":PEER,"body":"still here","created_at":"2024-01-02T03:04:06Z"}})).map_err(|err| err.to_string())?;
    until("duplicate preserved unread", &|| {
        Ok(
            matches!(ask(Request::Conversation { conversation:conversation.clone() })?,
        Response::Conversation { conversation:row } if row.preview == "still here" && row.unread == 2),
        )
    })?;
    let request = Request::Messages {
        conversation: conversation.clone(),
        page: Page::Newest,
        limit: 10,
    };
    ask(request.clone())?;
    histories
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "opening the DM did not request history".to_owned())?;
    until("opened DM history over IPC", &|| {
        Ok(
            matches!(ask(request.clone())?,Response::Messages { messages, .. }
        if messages.iter().any(|message| message.text == "history after opening")),
        )
    })?;
    eprintln!("direct-created: history fetched through daemon IPC and adapter");
    Ok(())
}
