//! Issue #757: Demo poll fixture — vote, count changes, vote again changes it back, and create a new poll.

use protocol::{
    read_frame, write_frame, ChatStream, Key, Network, Outgoing, Page, PollDraft, Request, Response,
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
            "/tmp/tinyclient-polls-{}-{}",
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
fn demo_poll_vote_retract_and_create() {
    let daemon = Daemon::demo();

    // Verify Discord demo account advertises polls capability.
    let status = daemon.ask(&Request::Status);
    let Response::Status { accounts, .. } = status else {
        panic!("expected Status, got {status:?}");
    };
    let discord = accounts
        .iter()
        .find(|a| a.key.network == Network::Discord)
        .expect("demo has Discord account");
    let polls_cap = discord.can.polls.expect("Discord demo has polls capability");
    assert!(polls_cap.max_options >= 2);

    // Find the seeded poll in #showcase.
    let showcase = Key::new(&discord.key, "400000000000000007");
    let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
        conversation: showcase.clone(),
        page: Page::Newest,
        limit: 50,
    }) else {
        panic!("expected Messages");
    };
    let poll_msg = messages
        .iter()
        .find(|m| m.poll.is_some())
        .expect("#showcase has a seeded poll message");
    let initial = poll_msg.poll.as_ref().unwrap();
    assert_eq!(initial.question, "Where should the autumn foray go?");
    assert_eq!(initial.choices.len(), 3);
    let c2_before = initial.choices[2].count;
    assert!(initial.mine.is_empty());

    // Vote for choice 2.
    assert_eq!(
        daemon.ask(&Request::Poll {
            conversation: showcase.clone(),
            message_id: poll_msg.id.clone(),
            votes: Some(vec![2]),
        }),
        Response::Ok
    );
    let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
        conversation: showcase.clone(),
        page: Page::Newest,
        limit: 50,
    }) else {
        panic!("expected Messages");
    };
    let updated = messages.iter().find(|m| m.id == poll_msg.id).unwrap().poll.as_ref().unwrap();
    assert_eq!(updated.mine, vec![2]);
    assert_eq!(updated.choices[2].count, c2_before + 1);

    // Retract vote (empty vec) -> count returns to before.
    assert_eq!(
        daemon.ask(&Request::Poll {
            conversation: showcase.clone(),
            message_id: poll_msg.id.clone(),
            votes: Some(vec![]),
        }),
        Response::Ok
    );
    let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
        conversation: showcase.clone(),
        page: Page::Newest,
        limit: 50,
    }) else {
        panic!("expected Messages");
    };
    let retracted = messages.iter().find(|m| m.id == poll_msg.id).unwrap().poll.as_ref().unwrap();
    assert!(retracted.mine.is_empty());
    assert_eq!(retracted.choices[2].count, c2_before);

    // Create a new poll via Request::Send with Outgoing::poll.
    let send = daemon.ask(&Request::Send {
        conversation: showcase.clone(),
        client_id: "poll-create-1".into(),
        text: "Poll: Coffee?".into(),
        reply_to: None,
        with: Outgoing {
            poll: Some(PollDraft {
                question: "Coffee?".into(),
                options: vec!["Espresso".into(), "Pour over".into()],
                multiple: true,
                duration_secs: Some(3600),
                anonymous: false,
            }),
            ..Outgoing::default()
        },
    });
    assert!(matches!(send, Response::Sent { .. } | Response::Ok), "got {send:?}");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut created_id = None;
    while Instant::now() < deadline {
        if let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
            conversation: showcase.clone(),
            page: Page::Newest,
            limit: 10,
        }) {
            if let Some(m) = messages.iter().find(|m| m.poll.as_ref().is_some_and(|p| p.question == "Coffee?")) {
                let p = m.poll.as_ref().unwrap();
                assert!(p.multiple);
                assert!(p.closes_at.is_some());
                assert_eq!(p.choices.len(), 2);
                created_id = Some(m.id.clone());
                break;
            }
        }
        sleep(Duration::from_millis(50));
    }
    let created_id = created_id.expect("created poll appears in conversation");

    // End the poll we created.
    assert_eq!(
        daemon.ask(&Request::Poll {
            conversation: showcase.clone(),
            message_id: created_id.clone(),
            votes: None,
        }),
        Response::Ok
    );
    let Response::Messages { messages, .. } = daemon.ask(&Request::Messages {
        conversation: showcase,
        page: Page::Newest,
        limit: 10,
    }) else {
        panic!("expected Messages");
    };
    let ended = messages.iter().find(|m| m.id == created_id).unwrap().poll.as_ref().unwrap();
    assert!(ended.closed);
}
