//! The Messages adapter through chatd's real socket, with a scripted phone
//! (`adapters/messages/fake.rs`). It proves what the adapter asks for and how that maps into
//! the contract. It proves nothing about Google's relay, which needs a real phone.

use protocol::{
    read_frame, write_frame, AccountKey, CallSupport, Change, ChatStream, ConversationKind,
    Encryption, ErrorCode, Key, Link, MediaKind, Message, MessageKind, Network, Notify, Outgoing,
    OutgoingFile, Page, ReactionSet, RelateAction, Request, Response, SendState, SharedKind, Shelf,
    SignInInput, SignInStep, Special,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const ME: &str = "phone-ada";
const GRACE: &str = "phone-grace";
const ALAN: &str = "phone-alan";
const CLUB: &str = "phone-club";
const SESSION: &str = r#"{"account":"phone-ada","name":"Ada Quill"}"#;

fn key(id: &str) -> Key {
    Key {
        network: Network::Messages,
        account: ME.into(),
        id: id.into(),
    }
}

fn account() -> AccountKey {
    AccountKey {
        network: Network::Messages,
        account: ME.into(),
    }
}

struct World {
    child: Child,
    dir: PathBuf,
}

impl World {
    fn linked() -> World {
        World::start(Some(SESSION))
    }

    /// A scripted conversation of 100 messages, so a scroll can ask for the pages under the newest.
    fn many() -> World {
        World::open(Some(SESSION), true, false)
    }

    /// The phone never answers, so sign-out cannot ask it to drop this computer.
    fn offline_phone() -> World {
        World::open(Some(SESSION), false, true)
    }

    fn start(session: Option<&str>) -> World {
        World::open(session, false, false)
    }

    fn open(session: Option<&str>, many: bool, offline: bool) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::path::absolute(format!(
            "/tmp/tinyclient-msg-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_chatd"));
        command
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_MESSAGES_FAKE", "1")
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .env_remove("TINYCLIENT_SESSION_TEAMS")
            .env_remove("TINYCLIENT_SESSION_SIGNAL")
            .env_remove("TINYCLIENT_SESSION_MESSAGES")
            .env_remove("TINYCLIENT_MESSAGES_OFFLINE")
            .stderr(Stdio::from(log));
        if offline {
            command.env("TINYCLIENT_MESSAGES_OFFLINE", "1");
        }
        if let Some(session) = session {
            command.env("TINYCLIENT_SESSION_MESSAGES", session);
        }
        if many {
            command.env("TINYCLIENT_MESSAGES_MANY", "100");
        }
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        World { child, dir }
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    fn until<T>(&self, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(found) = look() {
                return found;
            }
            if Instant::now() > deadline {
                panic!(
                    "waited for {what}; Messages was asked: {:#?}; chatd said: {}",
                    self.calls(),
                    self.stderr()
                );
            }
            sleep(Duration::from_millis(30));
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("messages.calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn heard(&self, line: &str) {
        self.until(&format!("Messages to hear {line:?}"), || {
            self.calls().iter().any(|call| call == line).then_some(())
        });
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.dir.join("stderr.log")).unwrap_or_default()
    }

    fn steer(&self, line: &str) {
        self.write_to("messages.ctl", line);
    }

    fn steer_link(&self, line: &str) {
        self.write_to("messages.signin.ctl", line);
    }

    fn write_to(&self, control: &str, line: &str) {
        let path = self.dir.join(control);
        self.until("the fake to listen", || path.exists().then_some(()));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
        drop(file);
        let end = std::fs::metadata(&path).unwrap().len();
        let mut taken = path.clone().into_os_string();
        taken.push(".taken");
        let taken = PathBuf::from(taken);
        let read = || {
            std::fs::read_to_string(&taken)
                .ok()
                .and_then(|at| at.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        self.until("the fake to take the line", || {
            (read() >= end).then_some(())
        });
    }

    fn status(&self) -> (Vec<protocol::AccountStatus>, u64) {
        match self.ask(Request::Status) {
            Response::Status { accounts, seq, .. } => (accounts, seq),
            other => panic!("{other:?}"),
        }
    }

    fn connected(&self) {
        self.until("Messages to connect", || {
            self.status()
                .0
                .iter()
                .any(|account| account.link == Link::Connected)
                .then_some(())
        });
        self.until("the group", || self.conversation(CLUB).map(drop));
        self.heard("list");
    }

    fn conversation(&self, id: &str) -> Option<protocol::Conversation> {
        match self.ask(Request::Conversation {
            conversation: key(id),
        }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    fn home(&self, shelf: Shelf) -> Vec<protocol::Conversation> {
        match self.ask(Request::Home {
            network: Network::Messages,
            account: Some(account()),
            folder: None,
            shelf,
            offset: 0,
            limit: 200,
        }) {
            Response::Conversations { conversations, .. } => conversations,
            other => panic!("{other:?}"),
        }
    }

    fn attention(&self) -> u32 {
        self.status()
            .0
            .iter()
            .find(|status| status.key == account())
            .map(|status| status.attention)
            .unwrap_or(0)
    }

    fn messages(&self, conversation: &str) -> Vec<Message> {
        self.page(conversation, Page::Newest, 100)
    }

    fn before(&self, id: &str) -> Vec<Message> {
        self.page(GRACE, Page::Before { id: id.to_owned() }, 40)
    }

    fn page(&self, conversation: &str, page: Page, limit: u32) -> Vec<Message> {
        match self.ask(Request::Messages {
            conversation: key(conversation),
            page,
            limit,
        }) {
            Response::Messages { messages, .. } => messages,
            _ => Vec::new(),
        }
    }

    fn lines(&self) -> Vec<Message> {
        self.messages(GRACE)
            .into_iter()
            .filter(|message| message.text.starts_with("line "))
            .collect()
    }

    fn arrived(&self, conversation: &str, text: &str) -> Message {
        self.until(&format!("message {text:?} in {conversation}"), || {
            self.messages(conversation)
                .into_iter()
                .find(|message| message.text == text)
        })
    }

    fn send(&self, conversation: &str, client_id: &str, text: &str) -> Message {
        self.send_reply(conversation, client_id, text, None)
    }

    fn send_reply(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>) -> Message {
        let asked = Request::Send {
            conversation: key(conversation),
            client_id: client_id.into(),
            text: text.into(),
            reply_to: reply_to.map(str::to_owned),
            with: Default::default(),
        };
        match self.ask(asked) {
            Response::Sent { message } => message,
            other => panic!("{other:?}"),
        }
    }

    fn changed<T>(
        &self,
        mut since: u64,
        what: &str,
        mut find: impl FnMut(&Change) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "no change: {what}; chatd said: {}",
                self.stderr()
            );
            let Response::Changes { seq, changes, .. } = self.ask(Request::Wait { since }) else {
                panic!("no changes");
            };
            since = seq;
            if let Some(found) = changes.iter().find_map(&mut find) {
                return found;
            }
        }
    }

    fn sign_in(&self, input: Option<SignInInput>) -> SignInStep {
        let request = match input {
            None => Request::SignInBegin {
                network: Network::Messages,
            },
            Some(input) => Request::SignInAnswer {
                network: Network::Messages,
                input,
            },
        };
        match self.ask(request) {
            Response::SignIn {
                network: Network::Messages,
                step,
            } => step,
            other => panic!("{other:?}"),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn a_paired_phone_lists_its_chats_and_a_text_arrives() {
    let world = World::linked();
    world.connected();
    let (accounts, _) = world.status();
    let mine = accounts
        .iter()
        .find(|status| status.key == account())
        .expect("the Messages account");
    assert_eq!((mine.name.as_str(), mine.me.as_str()), ("Ada Quill", ME));
    let club = world.conversation(CLUB).unwrap();
    assert_eq!(
        (club.kind, club.title.as_str(), club.unread),
        (ConversationKind::Group, "Bridge club", 0)
    );
    let grace = world.conversation(GRACE).unwrap();
    assert_eq!(
        (grace.kind, grace.title.as_str()),
        (ConversationKind::Direct, "Grace Ombra")
    );

    world.steer("incoming grace Are we still on for Tuesday?");
    let first = world.arrived(GRACE, "Are we still on for Tuesday?");
    assert_eq!(
        (first.sender.name.as_str(), first.outgoing),
        ("Grace Ombra", false)
    );

    let pending = world.send(GRACE, "1001", "Yes, seven sharp.");
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    world.heard(&format!("send {GRACE} Yes, seven sharp."));

    world.steer("photo grace");
    let photo = world.until("the photo", || {
        world.messages(GRACE).into_iter().find(|message| {
            message
                .attachments
                .iter()
                .any(|file| file.name == "photo.png")
        })
    });
    assert_eq!(photo.attachments.len(), 1);
    let file = &photo.attachments[0].file;
    assert_eq!(file.kind, MediaKind::Image);
    let path = file.path.as_deref().expect("the photo is on disk");
    assert_eq!(&std::fs::read(path).unwrap()[..8], b"\x89PNG\r\n\x1a\n");

    let Response::Reactions { rules, .. } = world.ask(Request::Reactions {
        conversation: key(GRACE),
    }) else {
        panic!("no rules");
    };
    assert_eq!(
        (rules.set, rules.mine, rules.replaces),
        (ReactionSet::Any, 1, true)
    );
}

#[test]
fn a_chat_on_screen_shows_the_picture_the_phone_keeps_for_its_person() {
    let world = World::linked();
    world.connected();
    world.until("Grace's chat", || world.conversation(GRACE).map(drop));
    assert!(world.conversation(GRACE).unwrap().avatar.is_none());
    let watching = protocol::Watching {
        rows: vec![key(GRACE), key(CLUB)],
        ..Default::default()
    };
    world.ask(Request::Watch(watching.clone()));
    // Only the one-to-one chat's person is asked about; a group has no picture of its own.
    world.heard("thumbnails participant-grace");
    let avatar = world.until("Grace's picture", || {
        world.conversation(GRACE).and_then(|row| row.avatar).filter(|file| file.path.is_some())
    });
    assert_eq!(avatar.kind, MediaKind::Avatar);
    assert_eq!(&std::fs::read(avatar.path.unwrap()).unwrap()[..4], b"\x89PNG");
    assert!(world.conversation(CLUB).unwrap().avatar.is_none());

    // It is not asked for twice on one connection.
    world.ask(Request::Watch(protocol::Watching { rows: vec![key(GRACE)], ..Default::default() }));
    world.ask(Request::Watch(watching));
    sleep(Duration::from_millis(300));
    let asked = world.calls().iter().filter(|call| call.starts_with("thumbnails")).count();
    assert_eq!(asked, 1);
    assert!(world.conversation(GRACE).unwrap().avatar.is_some());
}

#[test]
fn a_named_file_stays_a_line_a_caption_keeps_the_photo_and_a_later_file_is_fetched() {
    let world = World::linked();
    world.connected();
    world.steer("file grace report.pdf");
    let named = world.arrived(GRACE, "report.pdf");
    match named.kind {
        MessageKind::Unsupported { link } => assert!(link.contains(GRACE), "{link}"),
        other => panic!("{other:?}"),
    }
    assert!(named.attachments.is_empty());

    world.steer("caption grace look");
    let caption = world.arrived(GRACE, "look");
    assert_eq!(caption.attachments[0].file.kind, MediaKind::Image);
    assert!(caption.attachments[0].file.path.is_some());

    world.steer("remote grace");
    let remote = world.until("the remote photo", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|message| message.id.contains("remote"))
    });
    assert!(remote.attachments[0].file.path.is_none());
    let id = remote.attachments[0].file.id.clone();
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec![id.clone()],
        size: None,
        retry: false,
    });
    world.heard(&format!("fetch {} fake-photo", remote.id));
    world.until("the file on disk", || {
        match world.ask(Request::Fetch {
            account: account(),
            ids: vec![id.clone()],
            size: None,
            retry: false,
        }) {
            Response::Media { files } => files
                .into_iter()
                .find(|file| file.id == id)
                .and_then(|file| file.path)
                .filter(|path| std::fs::read(path).unwrap().starts_with(b"\x89PNG")),
            _ => None,
        }
    });
}

#[test]
fn a_reaction_is_shown_and_one_we_send_is_asked_of_the_phone() {
    let world = World::linked();
    world.connected();
    world.steer("react grace Thanks");
    let message = world.arrived(GRACE, "Thanks");
    assert_eq!(message.reactions.len(), 1);
    assert_eq!(
        (
            message.reactions[0].emoji.as_str(),
            message.reactions[0].count,
            message.reactions[0].mine
        ),
        ("👍", 1, false)
    );
    match world.ask(Request::React {
        conversation: key(GRACE),
        message_id: message.id.clone(),
        emoji: "😂".into(),
        on: true,
    }) {
        Response::Ok => {}
        other => panic!("{other:?}"),
    }
    world.heard(&format!("react {} 😂 1", message.id));
    match world.ask(Request::React {
        conversation: key(GRACE),
        message_id: message.id.clone(),
        emoji: "😂".into(),
        on: false,
    }) {
        Response::Ok => {}
        other => panic!("{other:?}"),
    }
    world.heard(&format!("react {} 😂 0", message.id));
}

#[test]
fn a_muted_chat_records_no_notification_and_an_open_one_does() {
    let world = World::linked();
    world.connected();
    assert_eq!(
        world.ask(Request::SetMute {
            conversation: key(GRACE),
            notify: Notify::Nothing,
            until: None,
        }),
        Response::Ok
    );
    assert!(world.conversation(GRACE).unwrap().muted);

    world.steer("incoming grace still tuesday?");
    world.arrived(GRACE, "still tuesday?");
    assert_eq!(world.calls().iter().filter(|call| *call == &format!("notify {GRACE}")).count(), 0);

    world.steer("group grace the cards");
    world.arrived(CLUB, "the cards");
    world.heard(&format!("notify {CLUB}"));

    assert_eq!(
        world.ask(Request::SetMute {
            conversation: key(GRACE),
            notify: Notify::All,
            until: None,
        }),
        Response::Ok
    );
    assert!(!world.conversation(GRACE).unwrap().muted);
    world.steer("incoming grace now it rings");
    world.arrived(GRACE, "now it rings");
    world.heard(&format!("notify {GRACE}"));

    assert_eq!(
        world.ask(Request::SetMute {
            conversation: key(GRACE),
            notify: Notify::Mentions,
            until: None,
        }),
        Response::Ok
    );
    assert!(world.conversation(GRACE).unwrap().muted);
    let before = world.calls().iter().filter(|call| *call == &format!("notify {GRACE}")).count();
    world.steer("incoming grace mentions stay quiet");
    world.arrived(GRACE, "mentions stay quiet");
    assert_eq!(
        world.calls().iter().filter(|call| *call == &format!("notify {GRACE}")).count(),
        before
    );
}

#[test]
fn a_file_we_send_is_read_and_shown_back() {
    let world = World::linked();
    world.connected();
    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.txt");
    std::fs::write(&path, b"hello").unwrap();
    let asked = Request::Send {
        conversation: key(GRACE),
        client_id: "file-1".into(),
        text: "see this".into(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "note.txt".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    };
    match world.ask(asked) {
        Response::Sent { message } => {
            assert!(matches!(message.state, SendState::Pending), "{message:?}")
        }
        other => panic!("{other:?}"),
    }
    world.heard(&format!("file {GRACE} note.txt 5 see this"));
    let sent = world.until("the sent file", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "see this"
                && message
                    .attachments
                    .iter()
                    .any(|file| file.name == "note.txt" && file.file.id.starts_with("messages:"))
        })
    });
    let path = sent.attachments[0].file.path.as_deref().expect("sent file");
    assert_eq!(std::fs::read(path).unwrap(), b"hello");
    assert_eq!(
        (
            sent.attachments[0].file.kind,
            sent.attachments[0].mime.as_str()
        ),
        (MediaKind::File, "text/plain")
    );

    let photo = dir.join("shot.jpg");
    std::fs::write(&photo, b"\xff\xd8\xff jpeg").unwrap();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "file-2".into(),
        text: "the picture".into(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: photo.to_string_lossy().into_owned(),
                name: "shot.jpg".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    }) {
        Response::Sent { message } => {
            assert!(matches!(message.state, SendState::Pending), "{message:?}")
        }
        other => panic!("{other:?}"),
    }
    let shown = world.until("the sent photo", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "the picture"
                && message
                    .attachments
                    .iter()
                    .any(|file| file.name == "shot.jpg" && file.file.id.starts_with("messages:"))
        })
    });
    assert_eq!(
        (
            shown.attachments[0].file.kind,
            shown.attachments[0].mime.as_str()
        ),
        (MediaKind::Image, "image/jpeg")
    );
}

#[test]
fn a_mov_is_sent_as_a_video() {
    let world = World::linked();
    world.connected();
    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mov");
    let mut bytes = vec![0, 0, 0, 0x14];
    bytes.extend_from_slice(b"ftypqt  ");
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    std::fs::write(&path, &bytes).unwrap();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "file-mov".into(),
        text: "the clip".into(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "clip.mov".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    }) {
        Response::Sent { message } => {
            assert!(matches!(message.state, SendState::Pending), "{message:?}")
        }
        other => panic!("{other:?}"),
    }
    let shown = world.until("the sent clip", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "the clip"
                && message
                    .attachments
                    .iter()
                    .any(|file| file.name == "clip.mov" && file.file.id.starts_with("messages:"))
        })
    });
    assert_eq!(
        (
            shown.attachments[0].file.kind,
            shown.attachments[0].mime.as_str()
        ),
        (MediaKind::Video, "video/quicktime")
    );
}

#[test]
fn three_files_are_one_message_and_a_cancel_stops_the_send() {
    let world = World::linked();
    world.connected();
    let mine = world
        .status()
        .0
        .into_iter()
        .find(|status| status.key == account())
        .expect("the Messages account");
    assert_eq!(mine.can.max_files, 10);

    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    for (name, bytes) in [
        ("one.txt", b"aaa".as_slice()),
        ("two.txt", b"bbbb"),
        ("three.txt", b"ccccc"),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        files.push(OutgoingFile {
            path: path.to_string_lossy().into_owned(),
            name: name.into(),
            spoiler: false,
            as_file: false,
            original: false,
            media: None,
        });
    }
    let too_many = vec![files[0].clone(); 11];
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "too-many".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            files: too_many,
            ..Outgoing::default()
        },
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("10"), "{detail}"),
        other => panic!("{other:?}"),
    }

    let (_, seq) = world.status();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "three".into(),
        text: "picnic".into(),
        reply_to: None,
        with: Outgoing {
            files,
            ..Outgoing::default()
        },
    }) {
        Response::Sent { message } => {
            assert!(matches!(message.state, SendState::Pending), "{message:?}")
        }
        other => panic!("{other:?}"),
    }
    for (name, len) in [("one.txt", 3), ("two.txt", 4), ("three.txt", 5)] {
        world.heard(&format!("file {GRACE} {name} {len} picnic"));
    }
    let sent = world.until("the three files", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "picnic"
                && message.attachments.len() == 3
                && message
                    .attachments
                    .iter()
                    .all(|file| file.file.id.starts_with("messages:"))
        })
    });
    let one = sent
        .attachments
        .iter()
        .find(|file| file.name == "one.txt")
        .expect("one.txt");
    let path = one.file.path.as_deref().expect("one.txt on disk");
    assert_eq!(std::fs::read(path).unwrap(), b"aaa");
    world.changed(seq, "upload progress", |change| match change {
        Change::Upload {
            client_id,
            sent,
            total,
            ..
        } if client_id == "three" && *sent == *total && *total == 12 => Some(()),
        _ => None,
    });

    world.steer("hold");
    world.heard("hold");
    let later = dir.join("later.txt");
    std::fs::write(&later, b"zzzz").unwrap();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "stop".into(),
        text: "nope".into(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: later.to_string_lossy().into_owned(),
                name: "later.txt".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    }) {
        Response::Sent { .. } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        world.ask(Request::CancelSend {
            conversation: key(GRACE),
            client_id: "stop".into(),
        }),
        Response::Ok
    );
    world.steer("go");
    world.until("the cancelled send", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.client_id.as_deref() == Some("stop")
                && matches!(&message.state, SendState::Failed { reason } if reason == "Cancelled")
        })
    });
    assert!(
        !world.calls().iter().any(|call| call.contains("later.txt")),
        "{:?}",
        world.calls()
    );
}

#[test]
fn an_archived_chat_leaves_the_main_list_and_spam_does_not_notify() {
    let world = World::linked();
    world.connected();
    assert!(world.home(Shelf::Main).iter().any(|row| row.key.id == GRACE));

    world.steer("archive grace");
    world.until("Grace on the archive shelf", || {
        world
            .home(Shelf::Archive)
            .iter()
            .any(|row| row.key.id == GRACE)
            .then_some(())
    });
    assert!(world.home(Shelf::Main).iter().all(|row| row.key.id != GRACE));
    // A later update that does not repeat the status must not put the chat back.
    world.steer("again grace");
    world.until("a status-less update kept the archive", || {
        world
            .conversation(GRACE)
            .filter(|row| row.archived && row.title == "Grace Ombra still")
    });
    assert!(world.home(Shelf::Main).iter().all(|row| row.key.id != GRACE));

    let attention = world.attention();
    world.steer("spam alan");
    world.until("Alan behind requests", || {
        world
            .home(Shelf::Requests)
            .iter()
            .any(|row| row.key.id == ALAN)
            .then_some(())
    });
    assert!(world.home(Shelf::Main).iter().all(|row| row.key.id != ALAN));
    world.steer("incoming alan a blocked note");
    world.arrived(ALAN, "a blocked note");
    let spam = world.conversation(ALAN).expect("the spam chat");
    assert_eq!(spam.special, Special::Request);
    assert_eq!(
        world.attention(),
        attention,
        "a spam chat raises no badge and posts no notification"
    );
}

#[test]
fn blocking_a_person_asks_the_phone() {
    let world = World::linked();
    world.connected();
    match world.ask(Request::Relate {
        person: key(GRACE),
        action: RelateAction::Block,
    }) {
        Response::Ok => {}
        other => panic!("{other:?}"),
    }
    world.heard(&format!("block {GRACE}"));
    let blocked = world.until("Grace off the main list", || {
        world.conversation(GRACE).filter(|row| row.special == Special::Request)
    });
    assert!(blocked.special == Special::Request);
    assert!(world.home(Shelf::Main).iter().all(|row| row.key.id != GRACE));
}

#[test]
fn pairing_uses_google_browser_then_phone_emoji_confirmation() {
    let world = World::start(None);
    assert!(world
        .status()
        .0
        .iter()
        .all(|status| status.key.network != Network::Messages || status.link == Link::SignedOut));
    let (_, seq) = world.status();
    assert!(matches!(world.sign_in(None), SignInStep::Browser { waiting: false, .. }));
    world.sign_in(Some(SignInInput::OpenBrowser));
    let first = world.until("an emoji", || match world.sign_in(None) {
        SignInStep::Emoji { emoji } if !emoji.is_empty() => Some(emoji),
        _ => None,
    });
    assert!(
        !first.starts_with("https://"),
        "{first}"
    );
    let typed = world.sign_in(Some(SignInInput::UsePhone));
    assert!(
        matches!(&typed, SignInStep::Emoji { emoji } if *emoji == first),
        "{typed:?}"
    );
    world.steer_link("expire");
    let second = world.changed(seq, "a fresh emoji", |change| match change {
        Change::SignIn {
            network: Network::Messages,
            step: SignInStep::Emoji { emoji },
        } if !emoji.is_empty() && *emoji != first => Some(emoji.clone()),
        _ => None,
    });
    assert_ne!(first, second);

    let (_, seq) = world.status();
    world.steer_link("confirm");
    let done = world.changed(seq, "the emoji confirmation", |change| match change {
        Change::SignIn {
            network: Network::Messages,
            step: SignInStep::Done { account },
        } => Some(account.clone()),
        _ => None,
    });
    assert_eq!(done, Some(account()));
    world.connected();
}

#[test]
fn google_pairing_can_be_cancelled_and_restarted() {
    let world = World::start(None);
    assert!(matches!(world.sign_in(None), SignInStep::Browser { waiting: false, .. }));
    // Legacy QR requests must never begin the obsolete pairing flow.
    assert!(matches!(world.sign_in(Some(SignInInput::UseQr)), SignInStep::Browser { waiting: false, .. }));
    world.sign_in(Some(SignInInput::OpenBrowser));
    world.until("the emoji challenge", || matches!(world.sign_in(None), SignInStep::Emoji { .. }).then_some(()));
    world.ask(Request::SignInCancel { network: Network::Messages });
    assert!(matches!(world.sign_in(Some(SignInInput::OpenBrowser)), SignInStep::Failed { .. }));
    assert!(matches!(world.sign_in(None), SignInStep::Browser { waiting: false, .. }));
    world.heard("pairing stopped");
    world.sign_in(Some(SignInInput::OpenBrowser));
    world.until("the restarted emoji challenge", || matches!(world.sign_in(None), SignInStep::Emoji { .. }).then_some(()));
    let (_, seq) = world.status();
    world.steer_link("confirm");
    world.changed(seq, "restarted pairing completion", |change| match change {
        Change::SignIn { network: Network::Messages, step: SignInStep::Done { .. } } => Some(()),
        _ => None,
    });
    world.connected();
}

#[test]
fn google_pairing_failure_is_reported_without_adopting_an_account() {
    let world = World::start(None);
    world.sign_in(None);
    world.sign_in(Some(SignInInput::OpenBrowser));
    world.until("the emoji challenge", || matches!(world.sign_in(None), SignInStep::Emoji { .. }).then_some(()));
    let (_, seq) = world.status();
    world.steer_link("fail");
    world.changed(seq, "pairing failure", |change| match change {
        Change::SignIn { network: Network::Messages, step: SignInStep::Failed { reason } } => Some(reason.clone()),
        _ => None,
    });
    assert!(world.status().0.iter().all(|status| status.key.network != Network::Messages || status.link == Link::SignedOut));
}

#[test]
fn an_sms_chat_says_text_and_an_encrypted_rcs_chat_does_not_claim_this_computer_holds_the_keys() {
    let world = World::linked();
    world.connected();
    world.steer("sms grace");
    let sms = world.until("the text label", || {
        world
            .conversation(GRACE)
            .filter(|row| row.topic == "Text message")
    });
    assert_eq!(sms.encryption, Encryption::Unknown);
    world.ask(Request::Typing {
        conversation: key(GRACE),
        activity: protocol::Compose::Typing,
    });
    let quiet = Instant::now() + Duration::from_secs(1);
    while Instant::now() < quiet {
        assert!(
            !world.calls().iter().any(|call| call.starts_with("typing ")),
            "an SMS chat told the phone we were typing"
        );
        sleep(Duration::from_millis(50));
    }
    world.steer("tick grace");
    sleep(Duration::from_millis(400));
    assert!(!world.conversation(GRACE).unwrap().seen);

    world.steer("sealed grace");
    let rcs = world.until("the encrypted RCS label", || {
        world.conversation(GRACE).filter(|row| {
            row.topic.contains("RCS message") && row.topic.contains("Encrypted between phones")
        })
    });
    assert_eq!(rcs.encryption, Encryption::Unknown);
    world.ask(Request::Typing {
        conversation: key(GRACE),
        activity: protocol::Compose::Typing,
    });
    world.heard(&format!("typing {GRACE}"));
    world.steer("tick grace");
    world.until("the read tick", || {
        world.conversation(GRACE).filter(|row| row.seen)
    });

    world.steer("mms grace a picture");
    world.until("the MMS tag", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|message| message.text == "MMS" && message.kind == MessageKind::System)
    });

    world.steer("sms grace");
    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.txt");
    std::fs::write(&path, b"hello").unwrap();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "mms-warn".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "note.txt".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    }) {
        Response::Sent { .. } => {}
        other => panic!("{other:?}"),
    }
    world.until("the MMS warning", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|message| message.text == "Sends as MMS; may be compressed")
    });
}

#[test]
fn a_quiet_phone_holds_a_text_until_the_phone_answers() {
    chatd::adapters::messages::prove_phone_quiet().expect("the quiet-phone check");
    let world = World::linked();
    world.connected();
    world.steer("silent");
    world.heard("silent");
    let while_quiet = world.calls().iter().filter(|call| call.starts_with("history ")).count();
    let pending = world.send(GRACE, "held-1", "On my way.");
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    let waiting = |text: &str| {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == text
                && matches!(&message.state, SendState::Failed { reason } if reason == chatd::adapters::messages::WAITING)
        })
    };
    world.until("the first text to wait", || waiting("On my way."));
    world.send(GRACE, "held-2", "Still here.");
    world.until("the second text to wait", || waiting("Still here."));
    world.until("the phone banner while the relay stays up", || {
        let (accounts, _) = world.status();
        accounts.into_iter().find(|status| status.key.account == ME).and_then(|status| {
            (status.link == Link::Connected
                && status.required == chatd::adapters::messages::CANT_REACH)
                .then_some(())
        })
    });
    assert_eq!(
        world.calls().iter().filter(|call| call.starts_with("history ")).count(),
        while_quiet,
        "a quiet phone was asked for history: {:?}",
        world.calls()
    );
    assert!(
        world.calls().iter().all(|call| !call.starts_with("send ")),
        "a quiet phone was asked to send: {:?}",
        world.calls()
    );
    world.steer("answer");
    world.until("the held texts to go, then the history", || {
        let calls = world.calls();
        let first = calls.iter().position(|call| call == "send phone-grace On my way.")?;
        let second = calls.iter().position(|call| call == "send phone-grace Still here.")?;
        let between = calls.iter().enumerate().any(|(index, call)| {
            index > first && index < second && call.starts_with("history ")
        });
        let later = calls
            .iter()
            .enumerate()
            .any(|(index, call)| index > second && call.starts_with("history "));
        let earlier = calls.iter().take(first).any(|call| call.starts_with("history "));
        (first < second && !between && (later || earlier)).then_some(())
    });
    world.until("both texts to be sent", || {
        let messages = world.messages(GRACE);
        let first = messages.iter().find(|message| message.text == "On my way.")?;
        let second = messages.iter().find(|message| message.text == "Still here.")?;
        (first.state == SendState::Sent && second.state == SendState::Sent).then_some(())
    });
    world.until("the banner to clear", || {
        let (accounts, _) = world.status();
        accounts.into_iter().find(|status| status.key.account == ME).and_then(|status| {
            (status.link == Link::Connected && status.required.is_empty()).then_some(())
        })
    });
}

#[test]
fn scrolling_back_reads_every_message_and_then_stops_asking() {
    let world = World::many();
    world.connected();
    let oldest = |messages: &[Message]| {
        messages
            .iter()
            .min_by_key(|message| message.sent)
            .map(|message| message.id.clone())
            .unwrap_or_default()
    };
    let first = world.until("the newest forty", || {
        let lines = world.lines();
        (lines.len() >= 40).then_some(lines)
    });
    let _ = world.before(&oldest(&first));
    world.until("the next forty", || (world.lines().len() >= 80).then_some(()));
    let mid = world.lines();
    let _ = world.before(&oldest(&mid));
    let all = world.until("all one hundred", || {
        let lines = world.lines();
        (lines.len() == 100).then_some(lines)
    });
    assert_eq!(all.first().map(|message| message.text.as_str()), Some("line 1"));
    assert_eq!(all.last().map(|message| message.text.as_str()), Some("line 100"));
    let calls = world.calls();
    assert!(
        calls.iter().any(|call| call == "history phone-grace m-0061"),
        "the second page did not start after the first: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| call == "history phone-grace m-0021"),
        "the last page did not start after the second: {calls:?}"
    );
    let asked = world
        .calls()
        .iter()
        .filter(|call| call.starts_with("history phone-grace"))
        .count();
    let _ = world.before(&oldest(&all));
    world.ask(Request::Typing {
        conversation: key(GRACE),
        activity: protocol::Compose::Typing,
    });
    world.heard(&format!("typing {GRACE}"));
    let after = world
        .calls()
        .iter()
        .filter(|call| call.starts_with("history phone-grace"))
        .count();
    assert_eq!(
        after, asked,
        "a third scroll asked the phone again: {:?}",
        world.calls()
    );
}

#[test]
fn an_rcs_reply_quotes_the_original_and_an_sms_reply_stays_plain() {
    let world = World::linked();
    world.connected();
    world.steer("rcs grace");
    world.until("the RCS label", || {
        world.conversation(GRACE).filter(|row| row.topic == "RCS message")
    });
    world.steer("incoming grace See you at seven");
    let original = world.arrived(GRACE, "See you at seven");

    let _ = world.send_reply(GRACE, "reply-1", "On my way", Some(&original.id));
    let sent = world.until("the reply to land", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "On my way" && message.state == SendState::Sent
        })
    });
    let quote = sent.reply_to.expect("the sent reply keeps its quote");
    assert_eq!(quote.id, original.id);
    assert_eq!(quote.snippet, "See you at seven");
    assert_eq!(quote.sender, "Grace Ombra");
    world.heard(&format!("reply {GRACE} {}", original.id));
    world.heard(&format!("send {GRACE} On my way"));

    world.steer(&format!("quote grace {} I'll be there", original.id));
    let heard = world.arrived(GRACE, "I'll be there");
    let heard_quote = heard.reply_to.expect("the incoming reply shows its quote");
    assert_eq!(heard_quote.id, original.id);
    assert_eq!(heard_quote.snippet, "See you at seven");
    assert_eq!(heard_quote.sender, "Grace Ombra");

    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.txt");
    std::fs::write(&path, b"hello").unwrap();
    match world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "reply-file".into(),
        text: "the notes".into(),
        reply_to: Some(original.id.clone()),
        with: Outgoing {
            files: vec![OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "note.txt".into(),
                spoiler: false,
                as_file: false,
                original: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    }) {
        Response::Sent { .. } => {}
        other => panic!("{other:?}"),
    }
    world.heard(&format!("file {GRACE} note.txt 5 the notes"));
    world.heard(&format!("reply {GRACE} {}", original.id));
    let filed = world.until("the file reply to land", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "the notes" && message.state == SendState::Sent
        })
    });
    assert_eq!(filed.reply_to.as_ref().map(|quote| quote.id.as_str()), Some(original.id.as_str()));

    world.steer("sms grace");
    world.until("the text label", || {
        world.conversation(GRACE).filter(|row| row.topic == "Text message")
    });
    let before = world.calls().iter().filter(|call| call.starts_with("reply ")).count();
    let _ = world.send_reply(GRACE, "reply-sms", "Still coming", Some(&original.id));
    let plain = world.until("the SMS send to land", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "Still coming" && message.state == SendState::Sent
        })
    });
    assert!(plain.reply_to.is_none(), "an SMS send kept a quote the phone cannot carry: {plain:?}");
    world.heard(&format!("send {GRACE} Still coming"));
    let after = world.calls().iter().filter(|call| call.starts_with("reply ")).count();
    assert_eq!(after, before, "an SMS chat was handed a reply reference: {:?}", world.calls());
}

#[test]
fn a_revoked_phone_signs_out_and_does_not_connect_again() {
    let world = World::linked();
    world.connected();
    let lists = world.calls().iter().filter(|call| *call == "list").count();
    world.steer("revoke");
    world.until("Messages to ask to pair again", || {
        world.status().0.into_iter().find(|account| {
            account.key.network == Network::Messages
                && account.link == Link::Expired
                && account.detail.contains("Pair again")
        })
    });
    sleep(Duration::from_millis(400));
    let later = world.calls().iter().filter(|call| *call == "list").count();
    assert_eq!(later, lists);
}

#[test]
fn a_phone_revoked_while_offline_does_not_keep_connecting() {
    let world = World::linked();
    world.connected();
    world.steer("revoke-offline");
    world.until("Messages to ask to pair again", || {
        world.status().0.into_iter().find(|account| {
            account.key.network == Network::Messages
                && account.link == Link::Expired
                && account.detail.contains("Pair again")
        })
    });
    let connects = world.calls().iter().filter(|call| *call == "connect").count();
    assert_eq!(connects, 1);
    sleep(Duration::from_millis(400));
    let connects = world.calls().iter().filter(|call| *call == "connect").count();
    assert_eq!(connects, 1);
}

#[test]
fn two_auth_refusals_sign_out_and_one_does_not() {
    let world = World::linked();
    world.connected();
    let lists = world.calls().iter().filter(|call| *call == "list").count();
    world.steer("auth");
    world.until("Messages to connect again after one refusal", || {
        let listed = world.calls().iter().filter(|call| *call == "list").count();
        (listed > lists).then_some(())
    });
    assert!(world.status().0.iter().all(|account| account.link != Link::Expired));
    world.steer("auth");
    world.until("Messages to ask to pair again", || {
        world.status().0.into_iter().find(|account| {
            account.key.network == Network::Messages
                && account.link == Link::Expired
                && account.detail.contains("Pair again")
        })
    });
}

#[test]
fn a_failed_send_is_one_message_and_try_again_uses_the_phone_copy() {
    let world = World::linked();
    world.connected();
    let pending = world.send(GRACE, "c-fail", "too big");
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    world.heard(&format!("send {GRACE} too big"));
    world.until("the pending row", || {
        world.messages(GRACE).into_iter().find(|message| message.id == "c-fail" && message.text == "too big")
    });
    world.steer("status 13");
    let failed = world.until("the failure", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.text == "too big" && matches!(message.state, SendState::Failed { .. })
        })
    });
    assert_eq!(failed.id, "c-fail");
    match &failed.state {
        SendState::Failed { reason } => assert!(reason.contains("too large"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(world.messages(GRACE).iter().filter(|message| message.text == "too big").count(), 1);
    let sends = world.calls().iter().filter(|call| call.starts_with("send ")).count();
    match world.ask(Request::Retry { conversation: key(GRACE), client_id: "c-fail".into() }) {
        Response::Sent { message } => assert!(matches!(message.state, SendState::Pending), "{message:?}"),
        other => panic!("{other:?}"),
    }
    world.until("the phone copy is sent again", || {
        world.calls().iter().any(|call| call.starts_with("resend phone-")).then_some(())
    });
    let calls = world.calls();
    assert_eq!(calls.iter().filter(|call| call.starts_with("send ")).count(), sends);
    assert_eq!(calls.iter().filter(|call| call.starts_with("resend ")).count(), 1);

    world.send(GRACE, "c-wait", "on its way");
    world.heard(&format!("send {GRACE} on its way"));
    world.until("the waiting row", || world.messages(GRACE).into_iter().find(|message| message.id == "c-wait"));
    world.steer("status 5");
    world.until("still sending", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.id == "c-wait" && matches!(message.state, SendState::Pending)
        })
    });
    world.steer("status 8");
    let quiet = world.until("no service", || {
        world.messages(GRACE).into_iter().find(|message| {
            message.id == "c-wait" && matches!(message.state, SendState::Failed { .. })
        })
    });
    match quiet.state {
        SendState::Failed { reason } => assert!(reason.contains("No service"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(world.messages(GRACE).iter().filter(|message| message.id == "c-wait").count(), 1);
}

#[test]
fn a_text_deleted_on_the_phone_leaves_and_delete_reaches_the_phone() {
    let world = World::linked();
    world.connected();
    world.steer("incoming grace hi");
    let mine = world.arrived(GRACE, "hi");
    match world.ask(Request::Delete {
        conversation: key(GRACE),
        message_id: mine.id.clone(),
    }) {
        Response::Ok => {}
        other => panic!("{other:?}"),
    }
    world.heard(&format!("delete {}", mine.id));
    world.steer("incoming grace hello");
    let message = world.arrived(GRACE, "hello");
    world.steer(&format!("erase grace {}", message.id));
    world.until("the text deleted on the phone", || {
        (!world.messages(GRACE).iter().any(|item| item.id == message.id)).then_some(())
    });
    world.steer("joined grace");
    world.until("someone joined", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|item| item.text == "Someone joined" && item.kind == MessageKind::System)
    });
    world.steer("gone grace");
    world.until("the chat deleted on the phone", || match world.conversation(GRACE) {
        None => Some(()),
        Some(_) => None,
    });
}

#[test]
fn three_texts_count_as_three_and_a_chat_read_on_the_phone_clears() {
    let world = World::linked();
    world.connected();
    let club = world.conversation(CLUB).unwrap();
    assert_eq!(club.unread, 0, "a flag with no messages was shown as a count");
    for text in ["a", "bb", "ccc"] {
        world.steer(&format!("incoming grace {text}"));
        world.arrived(GRACE, text);
    }
    assert_eq!(world.conversation(GRACE).unwrap().unread, 3);
    world.steer("hold grace");
    let held = world.until("the unread flag", || {
        world.conversation(GRACE).filter(|row| row.title == "Held")
    });
    assert_eq!(held.unread, 3, "the unread flag replaced the count with 1");
    world.steer("read grace");
    let read = world.until("the chat read on the phone", || {
        world.conversation(GRACE).filter(|row| row.title == "Read")
    });
    assert_eq!(read.unread, 0);
}

#[test]
fn signing_out_asks_the_phone_to_drop_this_computer_once() {
    let world = World::linked();
    world.connected();
    match world.ask(Request::SignOut {
        account: account(),
        erase: false,
    }) {
        Response::Status { accounts, .. } => {
            assert!(
                accounts
                    .iter()
                    .all(|status| status.key != account() || status.link != Link::Connected),
                "{accounts:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    world.heard("unpair");
    assert_eq!(
        world.calls().iter().filter(|call| *call == "unpair").count(),
        1
    );
}

#[test]
fn signing_out_offline_says_the_phone_may_still_list_this_computer() {
    let world = World::offline_phone();
    // The socket answers before the account's threads exist. Wait until the phone is unreachable.
    world.heard("offline");
    match world.ask(Request::SignOut {
        account: account(),
        erase: false,
    }) {
        Response::Status { accounts, .. } => {
            assert!(
                accounts
                    .iter()
                    .all(|status| status.key != account() || status.link != Link::Connected),
                "{accounts:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    world.heard(
        "This computer may still be listed under Device pairing on your phone. Remove it there.",
    );
}

#[test]
fn a_video_is_a_video_and_a_voice_note_has_a_waveform() {
    chatd::adapters::messages::prove_message_media().expect("a video and a voice note");
    let world = World::linked();
    world.connected();

    world.steer("video grace");
    let clip = world.arrived(GRACE, "clip");
    let video = &clip.attachments[0];
    assert_eq!(video.file.kind, MediaKind::Video);
    assert_eq!(video.duration, 2);
    assert!(video.thumb.is_none());
    assert!(video.waveform.is_empty());
    assert!(video.file.path.is_some());

    world.steer("poster grace");
    let poster = world.arrived(GRACE, "poster");
    let shown = &poster.attachments[0];
    assert_eq!(shown.file.kind, MediaKind::Video);
    let thumb = shown.thumb.as_ref().expect("the still is the poster");
    assert_eq!(thumb.kind, MediaKind::Image);
    assert!(thumb.path.is_some());
    assert!(shown.file.path.is_none());

    world.steer("voice grace");
    let voice = world.arrived(GRACE, "voice");
    let note = &voice.attachments[0];
    assert_eq!(note.file.kind, MediaKind::Audio);
    assert_eq!(note.mime, "audio/amr");
    assert_eq!(note.duration, 1);
    assert!(note.waveform.iter().any(|bar| *bar != 0));
}

#[test]
fn an_older_line_is_found_without_opening_the_chat() {
    let world = World::many();
    world.connected();
    let hit = |query: &str, conversation: Option<Key>| match world.ask(Request::SearchMessages {
        conversation,
        network: Some(Network::Messages),
        space: None,
        query: query.into(),
        limit: 10,
    }) {
        Response::MessageHits { hits } => hits.into_iter().any(|hit| hit.message_id == "m-0011"),
        _ => false,
    };
    world.until("the 90th-newest line", || {
        hit("line 11", Some(key(GRACE))).then_some(())
    });
    assert!(
        hit("line 11", None),
        "the account search missed the older line: {:?}",
        world.calls()
    );
    world.heard("history phone-grace m-0021");
    let mut since = Instant::now();
    let mut seen = world.calls().len();
    world.until("the fill to stop", || {
        let now = world.calls().len();
        if now != seen {
            seen = now;
            since = Instant::now();
            return None;
        }
        (since.elapsed() > Duration::from_millis(500)).then_some(())
    });
    match world.ask(Request::Shared {
        conversation: key(GRACE),
        kind: SharedKind::Media,
        before: None,
        limit: 20,
        search: true,
    }) {
        Response::Shared { items, complete, .. } => {
            assert!(complete, "a finished chat was not the whole list");
            assert!(
                items.iter().any(|item| item.message_id == "m-0011"),
                "the older photo was not in the shared list: {items:?}"
            );
        }
        other => panic!("the shared list was not served: {other:?}"),
    }
}

#[test]
fn a_direct_messages_chat_hands_off_to_tel_and_a_group_has_no_call() {
    let world = World::linked();
    world.connected();
    let grace = world.conversation(GRACE).expect("Grace row");
    let club = world.conversation(CLUB).expect("Club row");
    assert_eq!(grace.call, CallSupport::Handoff);
    assert_eq!(club.call, CallSupport::None);

    match world.ask(Request::Handoff { conversation: key(GRACE) }) {
        Response::Link { url } => assert_eq!(url, "tel:+15550100002"),
        other => panic!("expected tel: handoff, got {other:?}"),
    }
    match world.ask(Request::Handoff { conversation: key(CLUB) }) {
        Response::Error { code: ErrorCode::Unsupported | ErrorCode::Refused, .. } => {}
        other => panic!("expected group handoff to be refused, got {other:?}"),
    }
}

#[test]
fn a_messages_group_answers_its_members_and_info() {
    let world = World::linked();
    world.connected();
    match world.ask(Request::Members {
        conversation: key(CLUB),
        offset: 0,
        limit: 20,
    }) {
        Response::Members { members, .. } => {
            assert_eq!(members.len(), 3, "expected 3 group members: {members:?}");
            let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
            assert!(names.contains(&"Ada Quill"), "{names:?}");
            assert!(names.contains(&"Grace Ombra"), "{names:?}");
            assert!(names.contains(&"Alan Ferro"), "{names:?}");
        }
        other => panic!("expected Members, got {other:?}"),
    }
    match world.ask(Request::ConversationInfo { conversation: key(CLUB) }) {
        Response::ConversationInfo { info } => {
            assert_eq!(info.members, Some(3));
        }
        other => panic!("expected ConversationInfo, got {other:?}"),
    }
}

#[test]
fn typing_resolves_a_raw_number_to_the_contact_name() {
    let world = World::linked();
    world.connected();

    let (_, since) = world.status();
    world.steer(&format!("typing-raw {GRACE} +15550100002"));
    let direct_typists = world.changed(since, "1:1 raw typing", |change| match change {
        Change::Typing { conversation, typists } if conversation.id == GRACE && !typists.is_empty() => Some(typists.clone()),
        _ => None,
    });
    assert_eq!(direct_typists.len(), 1);
    assert_eq!(direct_typists[0].id, GRACE);
    assert_eq!(direct_typists[0].name, "Grace Ombra");

    let (_, since) = world.status();
    world.steer(&format!("typing-raw {CLUB} +15550100002"));
    let group_typists = world.changed(since, "group raw typing", |change| match change {
        Change::Typing { conversation, typists } if conversation.id == CLUB && !typists.is_empty() => Some(typists.clone()),
        _ => None,
    });
    assert!(
        group_typists.iter().any(|t| t.name == "Grace Ombra"),
        "expected Grace Ombra in group typists: {group_typists:?}"
    );

    let (_, since) = world.status();
    world.steer(&format!("typing-raw {CLUB} +15550199999"));
    let unknown_typists = world.changed(since, "unknown raw typing", |change| match change {
        Change::Typing { conversation, typists } if conversation.id == CLUB && typists.iter().any(|t| t.name == "Someone" || t.name.contains("99999")) => Some(typists.clone()),
        _ => None,
    });
    assert!(
        unknown_typists.iter().any(|t| t.name == "Someone"),
        "expected unknown number to show Someone: {unknown_typists:?}"
    );
    assert!(
        !unknown_typists.iter().any(|t| t.name.contains("99999")),
        "raw phone number leaked into typist name: {unknown_typists:?}"
    );
}

#[test]
fn alphanumeric_senders_are_read_only_short_codes_stay_replyable_and_emergency_is_blocked() {
    let world = World::linked();
    world.connected();

    // Alphanumeric sender: read_only with exact reason, no call handoff.
    world.steer("noreply phone-bank BANKOTP");
    let bank = world.until("bank chat", || world.conversation("phone-bank"));
    assert!(bank.read_only, "alphanumeric sender should be read_only");
    assert_eq!(bank.read_only_reason, "This sender can't receive replies");
    assert_eq!(bank.call, CallSupport::None);
    match world.ask(Request::Send {
        conversation: key("phone-bank"),
        client_id: "no-reply-1".into(),
        text: "STOP".into(),
        reply_to: None,
        with: Default::default(),
    }) {
        Response::Error { detail, .. } => {
            assert!(detail.contains("can't receive replies"), "{detail}");
        }
        other => panic!("expected send to alphanumeric sender to be refused, got {other:?}"),
    }

    // Numeric short code: stays replyable (e.g. "STOP").
    world.steer("shortcode phone-short 89203");
    let short = world.until("short code chat", || world.conversation("phone-short"));
    assert!(!short.read_only, "numeric short code should stay replyable");

    // Verified business sender: Mark::Verified.
    world.steer("verified phone-biz Acme Bank");
    let biz = world.until("verified biz chat", || world.conversation("phone-biz"));
    assert_eq!(biz.mark, Some(protocol::Mark::Verified));

    // Emergency number: never offer call or text from tinyclient.
    world.steer("emergency phone-911 911");
    let em = world.until("emergency chat", || world.conversation("phone-911"));
    assert!(em.read_only, "emergency number must be read_only");
    assert_eq!(em.call, CallSupport::None);
    assert!(em.read_only_reason.contains("phone"), "{}", em.read_only_reason);
}

#[test]
fn rcs_messages_can_be_edited_inside_window_and_incoming_edits_are_marked() {
    let world = World::linked();
    world.connected();

    let (accounts, _) = world.status();
    let mine = accounts
        .iter()
        .find(|status| status.key == account())
        .expect("Messages account");
    assert_eq!(mine.can.edit_window, 15 * 60);

    // Switch Grace to RCS.
    world.steer("rcs grace");
    world.until("RCS topic", || {
        world
            .conversation(GRACE)
            .filter(|row| row.topic.contains("RCS message"))
    });

    // Incoming message edited by Grace is marked edited.
    world.steer("incoming grace Typo here");
    let incoming = world.arrived(GRACE, "Typo here");
    assert!(!incoming.edited);
    world.steer(&format!("edited grace {} Fixed text", incoming.id));
    let updated = world.until("incoming edit", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == incoming.id && m.edited)
    });
    assert_eq!(updated.text, "Fixed text");
    assert!(updated.edited);

    // Outgoing RCS message inside window can be edited.
    let sent = world.send(GRACE, "edit-1", "Original text");
    world.heard(&format!("send {GRACE} Original text"));
    world.until("sent settled", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == sent.id && m.state == SendState::Sent)
    });
    match world.ask(Request::Edit {
        conversation: key(GRACE),
        message_id: sent.id.clone(),
        text: "Edited text".into(),
        spans: Vec::new(),
    }) {
        Response::Ok => {}
        other => panic!("expected Edit Ok, got {other:?}"),
    }
    let edited = world.until("own edited message", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == sent.id && m.edited && m.text == "Edited text")
    });
    assert!(edited.edited);
    assert!(world.calls().iter().any(|c| c.starts_with(&format!("edit {GRACE} ")) && c.ends_with(" Edited text")));

    // Outgoing RCS message outside the 15-minute window is refused.
    world.steer("old-mine grace m-too-old 1800 Way too old");
    let old = world.arrived(GRACE, "Way too old");
    match world.ask(Request::Edit {
        conversation: key(GRACE),
        message_id: old.id.clone(),
        text: "Cannot edit".into(),
        spans: Vec::new(),
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("no longer be edited"), "{detail}"),
        other => panic!("expected stale edit refusal, got {other:?}"),
    }

    // SMS chat refuses edits.
    world.steer("sms grace");
    world.until("SMS topic", || {
        world
            .conversation(GRACE)
            .filter(|row| row.topic == "Text message")
    });
    match world.ask(Request::Edit {
        conversation: key(GRACE),
        message_id: sent.id.clone(),
        text: "SMS edit".into(),
        spans: Vec::new(),
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("RCS"), "{detail}"),
        other => panic!("expected SMS edit refusal, got {other:?}"),
    }
}

#[test]
fn profile_shows_masked_number_open_group_fetches_member_avatars_and_contacts_search() {
    let world = World::linked();
    world.connected();

    // 1:1 profile shows masked phone number and accent colour.
    match world.ask(Request::Profile {
        person: key("participant-grace"),
        space: None,
    }) {
        Response::Profile { profile } => {
            assert_eq!(profile.contacts.len(), 1, "{:?}", profile.contacts);
            assert_eq!(profile.contacts[0].kind, protocol::ContactKind::Phone);
            assert_eq!(profile.contacts[0].value, "+15550100002");
            assert!(profile.contacts[0].masked, "phone number must be masked by default");
            assert_eq!(profile.accent, Some(0x34A853));
        }
        other => panic!("expected Profile, got {other:?}"),
    }

    // Opening the group requests thumbnails for group participants and populates member avatars.
    world.ask(Request::Watch(protocol::Watching {
        conversation: Some(key(CLUB)),
        ..Default::default()
    }));
    world.until("group participant avatars", || {
        match world.ask(Request::Members {
            conversation: key(CLUB),
            offset: 0,
            limit: 20,
        }) {
            Response::Members { members, .. } => {
                let alan = members.iter().find(|m| m.key.id == "participant-alan")?;
                let grace = members.iter().find(|m| m.key.id == "participant-grace")?;
                (alan.avatar.is_some() && grace.avatar.is_some()).then_some(())
            }
            _ => None,
        }
    });

    // Searching People queries the phone's address book and returns contacts with no existing chat.
    let _ = world.ask(Request::People {
        network: Some(Network::Messages),
        relation: None,
        query: Some("Linnea".into()),
        offset: 0,
        limit: 20,
    });
    world.heard("contacts");
    let linnea = world.until("Linnea Sol contact", || {
        match world.ask(Request::People {
            network: Some(Network::Messages),
            relation: None,
            query: Some("Linnea".into()),
            offset: 0,
            limit: 20,
        }) {
            Response::People { people, .. } => people.into_iter().find(|p| p.name == "Linnea Sol"),
            _ => None,
        }
    });
    assert_eq!(linnea.key.id, "participant-linnea");
}

#[test]
fn start_direct_opens_a_chat_for_a_typed_number_or_contact_and_blocks_emergency() {
    let world = World::linked();
    world.connected();

    // StartDirect with a typed phone number normalizes to E.164 and creates a row with RCS topic.
    match world.ask(Request::StartDirect {
        person: key("(555) 010-0099"),
    }) {
        Response::Conversation { conversation } => {
            assert_eq!(conversation.kind, ConversationKind::Direct);
            assert!(conversation.topic.contains("RCS message"), "{}", conversation.topic);
            let stored = world.conversation(&conversation.key.id).expect("new row stored");
            assert_eq!(stored.key, conversation.key);
        }
        other => panic!("expected Conversation, got {other:?}"),
    }
    world.heard("create +15550100099");

    // StartDirect with a phone contact id resolves their number and creates a chat.
    match world.ask(Request::StartDirect {
        person: key("participant-linnea"),
    }) {
        Response::Conversation { conversation } => {
            assert_eq!(conversation.title, "Linnea Sol");
        }
        other => panic!("expected Conversation for Linnea, got {other:?}"),
    }
    world.heard("create +15550100004");

    // StartDirect with an emergency number is refused.
    match world.ask(Request::StartDirect {
        person: key("911"),
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("phone"), "{detail}"),
        other => panic!("expected emergency refusal, got {other:?}"),
    }
}

#[test]
fn create_group_starts_a_named_group_conversation_with_multiple_numbers() {
    let world = World::linked();
    world.connected();

    match world.ask(Request::CreateGroup {
        account: account(),
        members: vec![
            "participant-grace".into(),
            "participant-alan".into(),
            "(555) 010-0099".into(),
        ],
        name: Some("Weekend trip".into()),
    }) {
        Response::Conversation { conversation } => {
            assert_eq!(conversation.kind, ConversationKind::Group);
            assert_eq!(conversation.title, "Weekend trip");
            assert!(conversation.topic.contains("RCS message"), "{}", conversation.topic);
            let stored = world.conversation(&conversation.key.id).expect("group row stored");
            assert_eq!(stored.title, "Weekend trip");
        }
        other => panic!("expected group Conversation, got {other:?}"),
    }
    world.heard("create +15550100002,+15550100003,+15550100099 Weekend trip");

    // Fewer than two members is refused.
    match world.ask(Request::CreateGroup {
        account: account(),
        members: vec!["+15550100002".into()],
        name: None,
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("two"), "{detail}"),
        other => panic!("expected too-few-members refusal, got {other:?}"),
    }
}

#[test]
fn link_previews_unfurl_for_known_contacts_and_skip_unknown_numbers_and_spam() {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_bg = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            hits_bg.fetch_add(1, Ordering::SeqCst);
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = r#"<!doctype html><html><head>
                <meta property="og:title" content="Bridge Schedule">
                <meta property="og:site_name" content="Club News">
                <meta property="og:description" content="Tuesday at seven.">
            </head><body>ok</body></html>"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let world = World::linked();
    world.connected();

    // 1. Unknown number triggers NO fetch by default.
    let unknown_url = format!("http://{addr}/phish");
    world.steer(&format!("unknown-link +15550199888 Verify now {unknown_url}"));
    let unk_msg = world.arrived("phone-unk-15550199888", &format!("Verify now {unknown_url}"));
    sleep(Duration::from_millis(250));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "unknown number must not trigger a link fetch by default");
    assert!(unk_msg.previews.is_empty());

    // 2. Known contact (Grace) unfurls the link into a preview card.
    let known_url = format!("http://{addr}/schedule");
    world.steer(&format!("incoming grace See {known_url}"));
    let with_card = world.until("link preview card", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text.contains(&known_url) && !m.previews.is_empty())
    });
    assert_eq!(with_card.previews[0].title, "Bridge Schedule");
    assert_eq!(with_card.previews[0].site, "Club News");
    assert_eq!(with_card.previews[0].description, "Tuesday at seven.");
    let hits_after_known = hits.load(Ordering::SeqCst);
    assert!(hits_after_known >= 1);

    // 3. Spam folder never unfurls, even for a previously named sender.
    world.steer("spam grace");
    world.until("Grace in spam", || {
        world
            .conversation(GRACE)
            .filter(|c| c.special == Special::Request)
    });
    let spam_url = format!("http://{addr}/spam-link");
    world.steer(&format!("incoming grace Win {spam_url}"));
    world.arrived(GRACE, &format!("Win {spam_url}"));
    sleep(Duration::from_millis(250));
    assert_eq!(hits.load(Ordering::SeqCst), hits_after_known, "spam conversation must never unfurl links");
}
