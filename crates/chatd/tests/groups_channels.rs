//! Issue #758: Create a group chat or a channel on any network (demo scenario).

use protocol::{
    read_frame, write_frame, ChatStream, ConversationKind, Key, Network, Outgoing, Page, Request, Response,
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
            "/tmp/tinyclient-grp-{}-{}",
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
fn start_group_and_send_message_and_create_channel() {
    let daemon = Daemon::demo();

    let Response::Status { accounts, .. } = daemon.ask(&Request::Status) else {
        panic!("expected Status");
    };
    let discord = accounts
        .iter()
        .find(|a| a.key.network == Network::Discord)
        .expect("demo Discord account");
    assert!(discord.can.groups >= 3, "Discord demo allows creating groups");

    // Pick three people on Discord and start a group.
    let Response::People { people, .. } = daemon.ask(&Request::People {
        network: Some(Network::Discord),
        relation: None,
        query: None,
        offset: 0,
        limit: 10,
    }) else {
        panic!("expected People");
    };
    assert!(people.len() >= 3);
    let three: Vec<Key> = people.iter().take(3).map(|p| p.key.clone()).collect();

    let Response::Conversation { conversation: group } = daemon.ask(&Request::StartGroup {
        account: discord.key.clone(),
        people: three,
        title: Some("Foray Crew".into()),
    }) else {
        panic!("expected Conversation from StartGroup");
    };
    assert_eq!(group.kind, ConversationKind::Group);
    assert_eq!(group.title, "Foray Crew");

    // Send a message into the newly created group.
    let sent = daemon.ask(&Request::Send {
        conversation: group.key.clone(),
        client_id: "grp-msg-1".into(),
        text: "Welcome to the foray crew!".into(),
        reply_to: None,
        with: Outgoing::default(),
    });
    assert!(matches!(sent, Response::Sent { .. } | Response::Ok), "got {sent:?}");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = false;
    while Instant::now() < deadline {
        if let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
            conversation: group.key.clone(),
            page: Page::Newest,
            limit: 10,
        }) {
            if messages.iter().any(|m| m.text == "Welcome to the foray crew!") {
                found = true;
                break;
            }
        }
        sleep(Duration::from_millis(50));
    }
    assert!(found, "message sent to new group should be stored");

    // Verify Moss & Lichen advertises can_create_channels and CreateChannel works.
    let Response::Spaces { spaces } = daemon.ask(&Request::Spaces {
        network: Some(Network::Discord),
    }) else {
        panic!("expected Spaces");
    };
    let moss = spaces
        .iter()
        .find(|s| s.can_create_channels)
        .expect("demo Space advertises can_create_channels");

    let Response::Conversation { conversation: chan } = daemon.ask(&Request::CreateChannel {
        space: moss.key.clone(),
        name: "field-notes".into(),
        kind: ConversationKind::Channel,
        private: true,
        group: None,
    }) else {
        panic!("expected Conversation from CreateChannel");
    };
    assert_eq!(chan.title, "field-notes");
    assert_eq!(chan.kind, ConversationKind::Channel);
    assert!(chan.private);
    assert_eq!(chan.space.as_deref(), Some(moss.key.id.as_str()));
}
