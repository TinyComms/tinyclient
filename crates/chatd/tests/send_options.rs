//! Issue #776: Send options (importance, broadcast, message effects, view-once media, burst reactions).

use protocol::{
    read_frame, write_frame, ChatStream, ErrorCode, Importance, Key, Network, Outgoing, OutgoingFile, Page,
    Request, Response, SendState,
};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn demo() -> Daemon {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::path::absolute(format!(
            "/tmp/tinyclient-opts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .args(["--seed-demo"])
            .env("TINYCLIENT_DIR", &dir)
            .stderr(std::fs::File::create(dir.join("stderr")).unwrap())
            .spawn()
            .unwrap();
        let daemon = Daemon { child, dir };
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&daemon.dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        daemon
    }

    fn ask(&self, request: &Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, request).unwrap();
        read_frame(&mut stream).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn send_options_capabilities_and_delivery() {
    let daemon = Daemon::demo();

    let Response::Status { accounts, .. } = daemon.ask(&Request::Status) else {
        panic!("expected Status");
    };
    let discord = accounts.iter().find(|a| a.key.network == Network::Discord).unwrap();
    let slack = accounts.iter().find(|a| a.key.network == Network::Slack).unwrap();
    let telegram = accounts.iter().find(|a| a.key.network == Network::Telegram).unwrap();
    let teams = accounts.iter().find(|a| a.key.network == Network::Teams).unwrap();
    let signal = accounts.iter().find(|a| a.key.network == Network::Signal).unwrap();

    // Per-network capability flags are present only where supported.
    assert!(discord.can.burst_reactions && !discord.can.importance && !discord.can.effects);
    assert!(slack.can.thread_broadcast && !slack.can.burst_reactions && !slack.can.importance);
    assert!(telegram.can.effects && telegram.can.view_once && !telegram.can.importance);
    assert!(teams.can.importance && !teams.can.effects && !teams.can.burst_reactions);
    assert!(signal.can.view_once && !signal.can.importance);

    // Send a message with importance + effect + view_once attachment in demo #showcase.
    let showcase = Key::new(&discord.key, "400000000000000007");
    let outbox = daemon.dir.join("outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    let img = outbox.join("once.png");
    std::fs::write(&img, b"\x89PNG\r\n\x1a\n").unwrap();
    let send = daemon.ask(&Request::Send {
        conversation: showcase.clone(),
        client_id: "opt-1".into(),
        text: "Urgent update".into(),
        reply_to: None,
        with: Outgoing {
            importance: Importance::Urgent,
            effect: Some("confetti".into()),
            files: vec![OutgoingFile {
                path: img.to_string_lossy().into_owned(),
                view_once: true,
                ..OutgoingFile::default()
            }],
            ..Outgoing::default()
        },
    });
    assert!(matches!(send, Response::Sent { .. } | Response::Ok), "got {send:?}");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = None;
    while Instant::now() < deadline {
        if let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
            conversation: showcase.clone(),
            page: Page::Newest,
            limit: 10,
        }) {
            if let Some(m) = messages.iter().find(|m| m.text == "Urgent update" && m.state == SendState::Sent) {
                found = Some(m.clone());
                break;
            }
        }
        sleep(Duration::from_millis(50));
    }
    let msg = found.expect("sent message with options should appear");
    assert_eq!(msg.importance, Importance::Urgent);
    assert_eq!(msg.effect.as_deref(), Some("confetti"));
    assert_eq!(msg.attachments.len(), 1);
    assert!(msg.attachments[0].view_once);

    // Burst reaction succeeds on Discord demo (burst_reactions: true).
    assert_eq!(
        daemon.ask(&Request::React {
            conversation: showcase,
            message_id: msg.id.clone(),
            emoji: "🔥".into(),
            on: true,
            burst: true,
        }),
        Response::Ok
    );

    // Burst reaction is refused on Slack demo (burst_reactions: false).
    let slack_resp = daemon.ask(&Request::React {
        conversation: Key::new(&slack.key, "C04GENERAL"),
        message_id: "1".into(),
        emoji: "🔥".into(),
        on: true,
        burst: true,
    });
    assert!(
        matches!(slack_resp, Response::Error { code: ErrorCode::Unsupported, .. }),
        "expected Unsupported for burst reaction on Slack, got {slack_resp:?}"
    );
}
