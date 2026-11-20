//! The Signal adapter through chatd's real socket, with a scripted wire where Signal would be
//! (`adapters/signal/fake.rs`): the account, its contacts and its group are invented there, and
//! each test steers it by appending a line to a file in its scratch directory.
//!
//! What this proves is everything above the wire: what the adapter asks Signal for and how it maps
//! what comes back into the contract, and the link steps. It proves nothing about presage and
//! libsignal underneath, which no test can reach without a real account and a phone to link from.

use protocol::{
    read_frame, write_frame, AccountKey, Change, ChatStream, ContactKind, Conversation, ConversationKind, ErrorCode, Key, Link, Message,
    MessageKind, Network, Outgoing, OutgoingFile, Page, PollDraft, Relation, Request, Response, SendState, SignInInput, SignInProblem,
    SignInStep,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const ME: &str = "a1a1a1a1-0000-4000-8000-000000000001";
const GRACE: &str = "b2b2b2b2-0000-4000-8000-000000000002";
const ALAN: &str = "c3c3c3c3-0000-4000-8000-000000000003";
const CLUB: &str = "g.5a175a175a175a175a175a175a175a175a175a175a175a175a175a175a175a17";
const SESSION: &str = r#"{"aci":"a1a1a1a1-0000-4000-8000-000000000001","name":"Ada Quill","vault":"00"}"#;
/// 2026-10-13 12:00 UTC in milliseconds. Scripted lines that name this are outside the windows.
const T0: u64 = 1_791_892_800_000;

fn key(id: &str) -> Key {
    Key { network: Network::Signal, account: ME.into(), id: id.into() }
}

fn account() -> AccountKey {
    AccountKey { network: Network::Signal, account: ME.into() }
}

fn id(sent: u64, author: &str) -> String {
    format!("{sent}-{author}")
}

struct World {
    child: Child,
    dir: PathBuf,
}

impl World {
    /// chatd with the account already linked.
    fn linked() -> World {
        World::start(Some(SESSION))
    }

    fn start(session: Option<&str>) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-sg-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_chatd"));
        command
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_SIGNAL_FAKE", "1")
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .env_remove("TINYCLIENT_SESSION_TEAMS")
            .env_remove("TINYCLIENT_SESSION_SIGNAL")
            .stderr(Stdio::from(log));
        if let Some(session) = session {
            command.env("TINYCLIENT_SESSION_SIGNAL", session);
        }
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        World { child, dir }
    }

    /// Stops chatd and starts it again on the same directory, so a reaction remembered only in
    /// memory is gone and the store is what the next reaction has to replace against.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.dir.join("chat.sock"));
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("stderr.log")).unwrap();
        self.child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .env("TINYCLIENT_DIR", &self.dir)
            .env("TINYCLIENT_SIGNAL_FAKE", "1")
            .env("TINYCLIENT_SESSION_SIGNAL", SESSION)
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .env_remove("TINYCLIENT_SESSION_TEAMS")
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&self.dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket again: {}", self.stderr());
            sleep(Duration::from_millis(10));
        }
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
                panic!("waited for {what}; Signal was asked: {:#?}; chatd said: {}", self.calls(), self.stderr());
            }
            sleep(Duration::from_millis(30));
        }
    }

    /// Every request the fake was sent, one line each.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("signal.calls")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn heard(&self, line: &str) {
        self.until(&format!("Signal to hear {line:?}"), || self.calls().iter().any(|call| call == line).then_some(()));
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.dir.join("stderr.log")).unwrap_or_default()
    }

    /// Tells the fake what Signal says next.
    fn steer(&self, line: &str) {
        self.write_to("signal.ctl", line);
    }

    fn steer_link(&self, line: &str) {
        self.write_to("signal.signin.ctl", line);
    }

    fn write_to(&self, control: &str, line: &str) {
        let path = self.dir.join(control);
        self.until("the fake to listen", || path.exists().then_some(()));
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{line}").unwrap();
        drop(file);
        // The fake says how far it has read and acted: the line is taken when this returns.
        let end = std::fs::metadata(&path).unwrap().len();
        let mut taken = path.clone().into_os_string();
        taken.push(".taken");
        let taken = PathBuf::from(taken);
        let read = || std::fs::read_to_string(&taken).ok().and_then(|at| at.trim().parse::<u64>().ok()).unwrap_or(0);
        self.until("the fake to take the line", || (read() >= end).then_some(()));
    }

    fn status(&self) -> (Vec<protocol::AccountStatus>, u64) {
        match self.ask(Request::Status) {
            Response::Status { accounts, seq, .. } => (accounts, seq),
            other => panic!("{other:?}"),
        }
    }

    fn connected(&self) {
        self.until("Signal to connect", || self.status().0.iter().any(|account| account.link == Link::Connected).then_some(()));
        self.until("the group", || self.conversation(CLUB).map(drop));
    }

    fn conversation(&self, id: &str) -> Option<Conversation> {
        match self.ask(Request::Conversation { conversation: key(id) }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    fn messages(&self, conversation: &str) -> Vec<Message> {
        match self.ask(Request::Messages { conversation: key(conversation), page: Page::Newest, limit: 100 }) {
            Response::Messages { messages, .. } => messages,
            _ => Vec::new(),
        }
    }

    fn arrived(&self, conversation: &str, id: &str) -> Message {
        self.until(&format!("message {id} in {conversation}"), || self.messages(conversation).into_iter().find(|message| message.id == id))
    }

    fn send(&self, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>) -> Message {
        let asked = Request::Send { conversation: key(conversation), client_id: client_id.into(), text: text.into(), reply_to: reply_to.map(str::to_owned), with: Default::default() };
        match self.ask(asked) {
            Response::Sent { message } => message,
            other => panic!("{other:?}"),
        }
    }

    /// Waits on the change list from `since` until `find` picks one out.
    fn changed<T>(&self, mut since: u64, what: &str, mut find: impl FnMut(&Change) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "no change: {what}; chatd said: {}", self.stderr());
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
            None => Request::SignInBegin { network: Network::Signal },
            Some(input) => Request::SignInAnswer { network: Network::Signal, input },
        };
        match self.ask(request) {
            Response::SignIn { network: Network::Signal, step } => step,
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
fn a_linked_account_connects_with_its_contacts_and_groups() {
    let world = World::linked();
    world.connected();
    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|status| status.key == account()).expect("the Signal account");
    assert_eq!((signal.name.as_str(), signal.me.as_str()), ("Ada Quill", ME));
    let club = world.conversation(CLUB).unwrap();
    assert_eq!((club.kind, club.title.as_str(), club.topic.as_str()), (ConversationKind::Group, "Bridge club", "Tuesdays, 7 pm"));
    assert_eq!(club.key.network, Network::Signal);
    // Contacts are the user's own list; a person with no message has no row yet.
    let Response::People { people, .. } = world.ask(Request::People { network: Some(Network::Signal), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 }) else {
        panic!("no people");
    };
    let mut names: Vec<&str> = people.iter().map(|person| person.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Alan Ferro", "Grace Ombra"]);
    assert!(world.conversation(GRACE).is_none());
}

#[test]
fn a_person_on_screen_shows_their_profile_picture_and_keeps_it() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} Hello", T0 + 5_000));
    world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert!(world.conversation(GRACE).unwrap().avatar.is_none());
    let watching = protocol::Watching { rows: vec![key(GRACE), key(CLUB)], people: vec![key(ALAN)], ..Default::default() };
    world.ask(Request::Watch(watching.clone()));
    world.heard(&format!("picture {GRACE}"));
    world.heard(&format!("picture {CLUB}"));
    world.heard(&format!("picture {ALAN}"));
    let avatar = world.until("Grace's picture", || world.conversation(GRACE).and_then(|row| row.avatar).filter(|file| file.path.is_some()));
    assert_eq!(avatar.kind, protocol::MediaKind::Avatar);
    assert_eq!(&std::fs::read(avatar.path.as_deref().unwrap()).unwrap()[..4], b"\x89PNG");
    // Her person has it too; who has none, and the group, keep the drawn face.
    let Response::People { people, .. } = world.ask(Request::People { network: Some(Network::Signal), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 }) else {
        panic!("no people");
    };
    let grace = people.iter().find(|person| person.key.id == GRACE).unwrap();
    assert_eq!(grace.avatar.as_ref().map(|file| &file.id), Some(&avatar.id));
    assert!(people.iter().find(|person| person.key.id == ALAN).unwrap().avatar.is_none());
    assert!(world.conversation(CLUB).unwrap().avatar.is_none());

    // Another message leaves it, and it is asked for once a connection.
    world.steer(&format!("incoming grace {} Again", T0 + 6_000));
    world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    world.ask(Request::Watch(watching));
    sleep(Duration::from_millis(300));
    assert_eq!(world.calls().iter().filter(|call| call.starts_with("picture")).count(), 3);
    assert_eq!(world.conversation(GRACE).unwrap().avatar.map(|file| file.id), Some(avatar.id));
}

#[test]
fn text_is_received_sent_answered_and_a_refusal_is_told() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} Are we still on for Tuesday?", T0 + 5_000));
    let first = world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert_eq!((first.text.as_str(), first.sender.name.as_str(), first.sender.id.as_str(), first.outgoing), ("Are we still on for Tuesday?", "Grace Ombra", GRACE, false));
    assert_eq!(first.sent, ((T0 + 5_000) / 1_000) as i64);
    let row = world.conversation(GRACE).unwrap();
    assert_eq!((row.kind, row.title.as_str(), row.unread), (ConversationKind::Direct, "Grace Ombra", 1));

    let pending = world.send(GRACE, "1001", "Yes, seven sharp.", Some(&first.id));
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    let sent = world.until("our reply", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("1001") && message.id != "1001")
    });
    assert_eq!((sent.text.as_str(), sent.outgoing, sent.sender.name.as_str()), ("Yes, seven sharp.", true, "Ada Quill"));
    assert_eq!(sent.client_id.as_deref(), Some("1001"));
    let answered = sent.reply_to.expect("it answers Grace");
    assert_eq!((answered.id.as_str(), answered.sender.as_str(), answered.snippet.as_str()), (first.id.as_str(), "Grace Ombra", "Are we still on for Tuesday?"));
    world.heard(&format!("send {GRACE} text Yes, seven sharp. (answering {}-{GRACE})", T0 + 5_000));
    // Only one row for the message: the pending one became it.
    assert_eq!(world.messages(GRACE).iter().filter(|message| message.outgoing).count(), 1);

    world.steer("refuse Grace has blocked you.");
    world.send(GRACE, "1002", "Hello?", None);
    let failed = world.until("the refusal", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("1002") && matches!(message.state, SendState::Failed { .. }))
    });
    assert_eq!(failed.state, SendState::Failed { reason: "Grace has blocked you.".into() });

    // A message the user wrote on the phone arrives as ours.
    world.steer(&format!("mine {} On my way", T0 + 9_000));
    let mine = world.arrived(GRACE, &id(T0 + 9_000, ME));
    assert!(mine.outgoing && mine.sender.id == ME);
}

#[test]
fn reading_tells_the_phone_and_the_author_and_reading_on_the_phone_clears_the_row() {
    let world = World::linked();
    world.connected();
    for (at, text) in [(1, "one"), (2, "two"), (3, "three")] {
        world.steer(&format!("incoming alan {} {text}", T0 + at * 1_000));
    }
    world.arrived(ALAN, &id(T0 + 3_000, ALAN));
    world.until("three unread", || (world.conversation(ALAN)?.unread == 3).then_some(()));
    // Read up to the second: both are told, the third stays unread.
    assert_eq!(world.ask(Request::MarkRead { conversation: key(ALAN), message_id: id(T0 + 2_000, ALAN) }), Response::Ok);
    world.heard(&format!("read receipts=true {},{}", id(T0 + 1_000, ALAN), id(T0 + 2_000, ALAN)));
    // The user read the third on the phone.
    world.steer(&format!("read alan {}", T0 + 3_000));
    world.until("the row to be read", || (world.conversation(ALAN)?.unread == 0).then_some(()));
}

#[test]
fn a_group_mention_counts_and_what_cannot_be_drawn_says_so() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("group alan {} Who's bringing cards?", T0 + 1_000));
    let plain = world.arrived(CLUB, &id(T0 + 1_000, ALAN));
    assert!(!plain.mentions_me);
    world.steer(&format!("mention grace {} you're dealing", T0 + 2_000));
    let mention = world.arrived(CLUB, &id(T0 + 2_000, GRACE));
    assert!(mention.mentions_me && mention.text == "@Ada Quill you're dealing", "{mention:?}");
    assert_eq!(world.conversation(CLUB).unwrap().mentions, 1);
    world.steer(&format!("photo grace {}", T0 + 3_000));
    let photo = world.arrived(GRACE, &id(T0 + 3_000, GRACE));
    assert_eq!((photo.text.as_str(), &photo.kind), ("Photo", &MessageKind::Unsupported { link: String::new() }));
    // The group's reply goes to the group.
    world.send(CLUB, "2001", "I am.", None);
    world.heard(&format!("send {CLUB} text I am."));
}

#[test]
fn a_view_once_message_says_where_it_opens_instead_of_promising_the_media() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("viewonce grace {}", T0 + 4_000));
    let once = world.arrived(GRACE, &id(T0 + 4_000, GRACE));
    assert_eq!(
        (once.text.as_str(), &once.kind),
        ("View-once photo · open it on your phone", &MessageKind::Unsupported { link: String::new() })
    );
    // The media itself is not here: nothing to fetch, nothing to open twice.
    assert!(once.attachments.is_empty());
    world.steer(&format!("viewonce-video grace {}", T0 + 5_000));
    let video = world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert_eq!(video.text.as_str(), "View-once video · open it on your phone");
    // A photo without the flag is still only labelled.
    world.steer(&format!("photo grace {}", T0 + 6_000));
    let photo = world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    assert_eq!(photo.text.as_str(), "Photo");
}

#[test]
fn a_long_text_is_the_message_not_a_file() {
    let world = World::linked();
    world.connected();
    let body = "y".repeat(2_048);
    world.steer(&format!("longtext grace {} {body}", T0 + 4_000));
    let long = world.arrived(GRACE, &id(T0 + 4_000, GRACE));
    // What the body carried is the message, named as itself, with the note about the rest.
    assert_eq!(&long.kind, &MessageKind::Normal, "{long:?}");
    assert_eq!(long.text, format!("{body}\n\n(message continues on your phone)"));
    // And it reads like any message.
    world.send(GRACE, "5001", "Read it all.", None);
    world.heard(&format!("send {GRACE} text Read it all."));
}

#[test]
fn an_incoming_call_rings_and_leaves_a_missed_call_or_none_when_answered() {
    let world = World::linked();
    world.connected();
    // An offer is not a message: the row appears, with nothing in it and nothing unread.
    world.steer(&format!("call grace {}", T0 + 5_000));
    let row = world.until("Grace's row to be recorded", || world.conversation(GRACE));
    assert_eq!((row.kind, row.title.as_str(), row.unread), (ConversationKind::Direct, "Grace Ombra", 0));
    assert!(world.messages(GRACE).is_empty());
    // The caller gave up: one missed-call line, unread by one.
    world.steer(&format!("hangup {}", T0 + 5_000));
    let line = world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert_eq!((&line.kind, line.text.as_str(), line.outgoing, line.sender.id.as_str()), (&MessageKind::System, "Missed voice call", false, GRACE));
    world.until("one unread", || (world.conversation(GRACE)?.unread == 1).then_some(()));
    // A missed video call reads as one.
    world.steer(&format!("call-video grace {}", T0 + 6_000));
    world.steer(&format!("hangup {}", T0 + 6_000));
    let video = world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    assert_eq!((&video.kind, video.text.as_str()), (&MessageKind::System, "Missed video call"));
    // Answered on the phone: the ring ends and no line is left.
    world.steer(&format!("call alan {}", T0 + 7_000));
    world.until("Alan's row to be recorded", || world.conversation(ALAN).map(|row| row.unread == 0));
    world.steer(&format!("hangup-accepted {}", T0 + 7_000));
    sleep(Duration::from_millis(300));
    assert!(world.messages(ALAN).is_empty(), "an answered call is not a missed one");
    // A hangup for a call we never heard of leaves nothing.
    world.steer(&format!("busy {}", T0 + 8_000));
    sleep(Duration::from_millis(300));
    assert!(world.messages(ALAN).is_empty());
}

#[test]
fn a_newer_offer_takes_the_ring_and_only_its_own_hangup_ends_it() {
    let world = World::linked();
    world.connected();
    // Grace rings, then Alan's newer offer takes the one ring slot.
    world.steer(&format!("call grace {}", T0 + 5_000));
    world.until("Grace's row to be recorded", || world.conversation(GRACE));
    world.steer(&format!("call alan {}", T0 + 6_000));
    world.until("Alan's row to be recorded", || world.conversation(ALAN));
    // Grace's hangup is the displaced call's: it must not end Alan's ring or leave a line.
    world.steer(&format!("hangup {}", T0 + 5_000));
    sleep(Duration::from_millis(300));
    assert!(world.messages(GRACE).is_empty() && world.messages(ALAN).is_empty());
    // Only Alan's own hangup ends it: the caller gave up, so his missed-call line is left.
    world.steer(&format!("hangup {}", T0 + 6_000));
    let line = world.arrived(ALAN, &id(T0 + 6_000, ALAN));
    assert_eq!((&line.kind, line.text.as_str(), line.outgoing, line.sender.id.as_str()), (&MessageKind::System, "Missed voice call", false, ALAN));
    world.until("one unread", || (world.conversation(ALAN)?.unread == 1).then_some(()));
}

#[test]
fn reactions_edits_deletes_and_receipts_land_on_the_right_message() {
    let world = World::linked();
    world.connected();
    let lunch = now_ms();
    world.steer(&format!("incoming grace {lunch} Lunch?"));
    let theirs = world.arrived(GRACE, &id(lunch, GRACE));
    world.send(GRACE, "3001", "Sure", None);
    let ours = world.until("our message", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("3001") && message.id != "3001")
    });
    let ours_at = ours.id.split_once('-').unwrap().0;

    // Theirs: edited, reacted to (one reaction a person: the second replaces the first), deleted.
    world.steer(&format!("edit grace {lunch} Lunch at noon?"));
    world.until("the edit", || world.messages(GRACE).into_iter().find(|m| m.id == theirs.id && m.edited && m.text == "Lunch at noon?"));
    world.steer(&format!("react grace me {ours_at} 👍"));
    world.until("their reaction", || world.messages(GRACE).into_iter().find(|m| m.id == ours.id && m.reactions.iter().any(|r| r.emoji == "👍")));
    world.steer(&format!("react grace me {ours_at} ❤️"));
    let replaced = world.until("the second reaction", || {
        world.messages(GRACE).into_iter().find(|m| m.id == ours.id && m.reactions.iter().any(|r| r.emoji == "❤️"))
    });
    assert!(!replaced.reactions.iter().any(|r| r.emoji == "👍"), "{:?}", replaced.reactions);
    world.steer(&format!("delete grace {lunch}"));
    world.until("the delete", || (!world.messages(GRACE).iter().any(|m| m.id == theirs.id)).then_some(()));

    // Ours: the user reacts, edits and deletes, and only their own messages can be changed.
    world.steer(&format!("incoming grace {} Still there?", T0 + 2_000));
    let again = world.arrived(GRACE, &id(T0 + 2_000, GRACE));
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "👋".into(), on: true }), Response::Ok);
    world.heard(&format!("send {GRACE} react {}-{GRACE} 👋 remove=false", T0 + 2_000));
    // One reaction a person: another replaces mine at once and on Signal, and is the only one.
    let Response::Reactions { rules, .. } = world.ask(Request::Reactions { conversation: key(GRACE) }) else {
        panic!("no rules");
    };
    assert_eq!((rules.mine, rules.replaces, rules.custom), (1, true, protocol::EmojiScope::None));
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "🎉".into(), on: true }), Response::Ok);
    let held = world.messages(GRACE).into_iter().find(|m| m.id == again.id).unwrap();
    let mine: Vec<String> = held.reactions.into_iter().filter(|r| r.mine).map(|r| r.emoji).collect();
    assert_eq!(mine, ["🎉"]);
    world.heard(&format!("send {GRACE} react {}-{GRACE} 🎉 remove=false", T0 + 2_000));
    // A custom emoji is nothing Signal can carry, so it is never sent.
    let custom = world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: ":party:".into(), on: true });
    assert!(matches!(&custom, Response::Error { code: protocol::ErrorCode::Refused, .. }), "{custom:?}");
    assert!(!world.calls().iter().any(|call| call.contains(":party:")));
    assert_eq!(world.ask(Request::Edit { conversation: key(GRACE), message_id: ours.id.clone(), text: "Sure!".into() }), Response::Ok);
    world.heard(&format!("send {GRACE} edit {ours_at} Sure!"));
    assert!(world.messages(GRACE).iter().any(|m| m.id == ours.id && m.edited && m.text == "Sure!"));
    let refused = world.ask(Request::Edit { conversation: key(GRACE), message_id: again.id.clone(), text: "Not mine".into() });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail.contains("own messages")), "{refused:?}");
    // Grace's phone has ours, then Grace read it.
    assert!(!world.conversation(GRACE).unwrap().delivered);
    world.steer(&format!("delivered grace {ours_at}"));
    world.until("delivered", || world.conversation(GRACE)?.delivered.then_some(()));
    world.steer(&format!("seen grace {ours_at}"));
    world.until("seen", || world.conversation(GRACE)?.seen.then_some(()));
    assert_eq!(world.ask(Request::Delete { conversation: key(GRACE), message_id: ours.id.clone() }), Response::Ok);
    world.heard(&format!("send {GRACE} delete {ours_at}"));
}

#[test]
fn typing_is_heard_and_told_at_most_every_few_seconds() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} hi", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    let (_, seq) = world.status();
    world.steer("typing grace");
    let typists = world.changed(seq, "Grace typing", |change| match change {
        Change::Typing { conversation, typists } if conversation.id == GRACE => Some(typists.clone()),
        _ => None,
    });
    assert_eq!(typists.iter().map(|typist| typist.name.as_str()).collect::<Vec<_>>(), ["Grace Ombra"]);
    for _ in 0..3 {
        world.ask(Request::Typing { conversation: key(GRACE) });
    }
    world.heard(&format!("typing {GRACE} started"));
    sleep(Duration::from_millis(300));
    assert_eq!(world.calls().iter().filter(|call| call.starts_with("typing")).count(), 1);
}

#[test]
fn typing_and_receipts_have_a_local_switch_that_stops_the_frames_here() {
    let world = World::linked();
    world.connected();
    for (at, text) in [(1, "one"), (2, "two"), (3, "three")] {
        world.steer(&format!("incoming grace {} {text}", T0 + at * 1_000));
    }
    world.arrived(GRACE, &id(T0 + 3_000, GRACE));
    // Nothing stored: a read tells the author, as the account's own setting says.
    assert_eq!(world.ask(Request::MarkRead { conversation: key(GRACE), message_id: id(T0 + 1_000, GRACE) }), Response::Ok);
    world.heard(&format!("read receipts=true {}", id(T0 + 1_000, GRACE)));
    let scope = protocol::settings::account_scope(&account());
    let set = |key: &str, value: Option<&str>| Request::Set { scope: scope.clone(), key: key.to_owned(), value: value.map(str::to_owned) };
    // Receipts off: the chat is read here and nobody is told.
    assert_eq!(world.ask(set(protocol::settings::RECEIPTS, Some("off"))), Response::Ok);
    assert_eq!(world.ask(Request::MarkRead { conversation: key(GRACE), message_id: id(T0 + 2_000, GRACE) }), Response::Ok);
    sleep(Duration::from_millis(400));
    assert_eq!(world.calls().iter().filter(|call| call.starts_with("read ")).count(), 1, "a read went out with receipts off");
    // Typing off: the wire is never asked.
    assert_eq!(world.ask(set(protocol::settings::TYPING, Some("off"))), Response::Ok);
    world.ask(Request::Typing { conversation: key(GRACE) });
    sleep(Duration::from_millis(400));
    assert!(world.calls().iter().all(|call| !call.starts_with("typing")), "a typing signal went out with typing off");
    // Both back: the rows round-trip, and the frames go again.
    assert_eq!(world.ask(set(protocol::settings::RECEIPTS, None)), Response::Ok);
    assert_eq!(world.ask(set(protocol::settings::TYPING, None)), Response::Ok);
    assert_eq!(world.ask(Request::MarkRead { conversation: key(GRACE), message_id: id(T0 + 3_000, GRACE) }), Response::Ok);
    world.heard(&format!("read receipts=true {},{}", id(T0 + 2_000, GRACE), id(T0 + 3_000, GRACE)));
    // A fresh conversation, so the few-seconds memory of Grace's last one does not eat it.
    world.steer(&format!("incoming alan {} hello", T0 + 4_000));
    world.arrived(ALAN, &id(T0 + 4_000, ALAN));
    world.ask(Request::Typing { conversation: key(ALAN) });
    world.heard(&format!("typing {ALAN} started"));
}

#[test]
fn an_unlinked_device_expires_keeps_what_was_read_and_signing_out_unlinks() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} kept", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer("unlinked");
    let expired = world.until("the account to expire", || world.status().0.into_iter().find(|account| account.link == Link::Expired));
    assert_eq!(expired.detail, "This computer was removed from your Signal account.");
    assert_eq!(world.messages(GRACE).len(), 1);
    let refused = world.ask(Request::Send { conversation: key(GRACE), client_id: "4001".into(), text: "hello?".into(), reply_to: None, with: Default::default() });
    assert!(matches!(refused, Response::Error { code: ErrorCode::Offline, .. }), "{refused:?}");

    let world = World::linked();
    world.connected();
    world.ask(Request::SignOut { account: account(), erase: false });
    world.heard("unlink");
    assert!(world.conversation(CLUB).is_none());
}

#[test]
fn linking_shows_a_code_replaces_it_and_scanning_it_signs_in() {
    let world = World::start(None);
    assert!(world.status().0.iter().all(|status| status.key.network != Network::Signal || status.link == Link::SignedOut));
    let (_, seq) = world.status();
    assert!(matches!(world.sign_in(None), SignInStep::Qr { .. }));
    let first = world.until("a code", || match world.sign_in(None) {
        SignInStep::Qr { link, problem: None, .. } if !link.is_empty() => Some(link),
        _ => None,
    });
    assert!(first.starts_with("sgnl://linkdevice?"), "{first}");
    world.heard("link tinyclient");
    // There is no number to type: the code stays, and says why.
    let typed = world.sign_in(Some(SignInInput::UsePhone));
    assert!(matches!(&typed, SignInStep::Qr { link, problem: Some(SignInProblem::Other { .. }), .. } if *link == first), "{typed:?}");
    world.steer_link("expire");
    let second = world.changed(seq, "a fresh code", |change| match change {
        Change::SignIn { network: Network::Signal, step: SignInStep::Qr { link, .. } } if !link.is_empty() && *link != first => Some(link.clone()),
        _ => None,
    });
    assert_ne!(first, second);

    let (_, seq) = world.status();
    world.steer_link("scan");
    let done = world.changed(seq, "the scan", |change| match change {
        Change::SignIn { network: Network::Signal, step: SignInStep::Done { account } } => Some(account.clone()),
        _ => None,
    });
    assert_eq!(done, Some(account()));
    world.connected();
    assert_eq!(world.status().0.iter().find(|status| status.key == account()).unwrap().name, "Ada Quill");
    // Nothing of the session is in the log.
    assert!(!world.stderr().contains("vault"), "{}", world.stderr());
}

#[test]
fn a_cancelled_link_stops_and_a_new_one_begins_afresh() {
    let world = World::start(None);
    world.sign_in(None);
    world.heard("link tinyclient");
    assert_eq!(world.ask(Request::SignInCancel { network: Network::Signal }), Response::Ok);
    world.heard("link cancelled");
    // Answering a sign-in that is over says so.
    assert!(matches!(world.sign_in(Some(SignInInput::UseQr)), SignInStep::Failed { .. }));
    assert!(matches!(world.sign_in(None), SignInStep::Qr { .. }));
    world.ask(Request::SignInCancel { network: Network::Signal });
}

/// One pixel, so a send has a real picture to attach.
fn card_png() -> Vec<u8> {
    const PNG: &str = "89504e470d0a1a0a0000000d4948445200000001000000010802000000907753de0000000c4944415408d763f8cfc0000000020001e221bc330000000049454e44ae426082";
    (0..PNG.len()).step_by(2).map(|at| u8::from_str_radix(&PNG[at..at + 2], 16).unwrap()).collect()
}

#[test]
fn a_file_is_sent_with_its_attachment_and_the_phones_copy_does_not_replace_it() {
    let world = World::linked();
    world.connected();
    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|status| status.key == account()).expect("the Signal account");
    assert_eq!(signal.can.max_file_bytes, 100 * 1024 * 1024);
    assert_eq!(signal.can.max_files, 32);
    assert_eq!(signal.can.caption_limit, 0);

    // A direct chat exists once someone has written in it.
    world.steer(&format!("incoming grace {} hello", T0 + 5_000));
    world.arrived(GRACE, &id(T0 + 5_000, GRACE));

    let dir = world.dir.join("outbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("card.png");
    std::fs::write(&path, card_png()).unwrap();
    let asked = Request::Send {
        conversation: key(GRACE),
        client_id: "5001".into(),
        text: "the hand".into(),
        reply_to: None,
        with: protocol::Outgoing {
            files: vec![protocol::OutgoingFile {
                path: path.to_string_lossy().into_owned(),
                name: "card.png".into(),
                spoiler: false,
                as_file: false,
                original: false,
            }],
            ..protocol::Outgoing::default()
        },
    };
    let pending = match world.ask(asked) {
        Response::Sent { message } => message,
        other => panic!("{other:?}"),
    };
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    let sent = world.arrived(GRACE, &id(FIRST_SENT, ME));
    assert_eq!(sent.text.as_str(), "the hand");
    assert!(matches!(sent.state, SendState::Sent), "{sent:?}");
    assert_eq!(sent.attachments.len(), 1);
    let file = &sent.attachments[0];
    assert_eq!(file.name.as_str(), "card.png");
    assert_eq!(file.file.kind, protocol::MediaKind::Image);
    assert!(file.file.path.as_deref().is_some_and(|kept| std::fs::read(kept).is_ok_and(|bytes| !bytes.is_empty())));
    world.heard(&format!("send {GRACE} files card.png the hand"));
    assert_eq!(world.messages(GRACE).iter().filter(|message| message.outgoing).count(), 1);

    // The phone's sync of the same send must not become a second row, or wipe the file.
    world.steer(&format!("mine {FIRST_SENT} from the phone"));
    sleep(Duration::from_millis(400));
    let kept: Vec<Message> = world.messages(GRACE).into_iter().filter(|message| message.outgoing).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].text.as_str(), "the hand");
    assert_eq!(kept[0].attachments.len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn my_own_profile_shows_and_an_edit_reaches_the_wire() {
    let world = World::linked();
    world.connected();
    let Response::Profile { profile } = world.ask(Request::Profile { person: key(ME), space: None }) else {
        panic!("no card");
    };
    assert_eq!(profile.person.name, "Ada Quill");
    assert_eq!(profile.person.handle, "ada.12");
    assert_eq!(profile.person.link, "https://signal.me/#eu/fake-ada");
    assert_eq!(profile.bio, "Keeps the score.");
    assert!(profile.fields.iter().any(|(label, value)| label == "About emoji" && value == "🌉"), "{:?}", profile.fields);
    let phone = profile.contacts.iter().find(|contact| contact.kind == ContactKind::Phone).expect("a phone");
    assert_eq!((phone.value.as_str(), phone.masked), ("+15550100001", true));

    assert_eq!(
        world.ask(Request::SetProfile {
            account: account(),
            name: Some("Ada Q.".into()),
            about: Some("New about".into()),
            emoji: None,
            username: None,
            photo: false,
        }),
        Response::Ok
    );
    let wrote = world.calls().into_iter().filter(|line| line.contains("sync=local-profile")).collect::<Vec<_>>();
    assert_eq!(wrote.len(), 1, "{wrote:?}");
    assert!(wrote[0].contains("name=Ada Q.") && wrote[0].contains("about=New about"), "{}", wrote[0]);

    let Response::Profile { profile } = world.ask(Request::Profile { person: key(ME), space: None }) else {
        panic!("no card after the edit");
    };
    assert_eq!(profile.person.name, "Ada Q.");
    assert_eq!(profile.bio, "New about");
    assert_eq!(profile.person.handle, "ada.12");
    assert!(profile.fields.iter().any(|(label, value)| label == "About emoji" && value == "🌉"), "{:?}", profile.fields);
    assert_eq!(world.status().0.iter().find(|status| status.key == account()).unwrap().name, "Ada Q.");

    let username = world.ask(Request::SetProfile {
        account: account(),
        name: None,
        about: None,
        emoji: None,
        username: Some("nope".into()),
        photo: false,
    });
    match username {
        Response::Error { code: ErrorCode::Refused, detail } if detail.to_lowercase().contains("phone") => {}
        other => panic!("{other:?}"),
    }
    let photo = world.ask(Request::SetProfile {
        account: account(),
        name: None,
        about: None,
        emoji: None,
        username: None,
        photo: true,
    });
    match photo {
        Response::Error { code: ErrorCode::Refused, detail } if detail.to_lowercase().contains("phone") => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(world.calls().into_iter().filter(|line| line.contains("sync=local-profile")).count(), 1);
}

#[test]
fn note_to_self_from_the_phone_has_its_own_label() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("note {} Remember the tickets", T0 + 9_000));
    let mine = world.arrived(ME, &id(T0 + 9_000, ME));
    assert!(mine.outgoing && mine.sender.id == ME);
    assert_eq!(mine.sender.name, "Ada Quill");
    let row = world.conversation(ME).unwrap();
    assert_eq!((row.title.as_str(), row.kind, row.peer.as_deref()), ("Note to Self", ConversationKind::Direct, Some(ME)));

    // Simulate a row cached by a previous build, then reconnect without another note.
    let mut store = chatd::store::Store::open(&world.dir.join("chat.sqlite")).unwrap();
    let mut old = chatd::store::ConversationRow::new(ME, ConversationKind::Direct, "Signal user");
    old.peer = Some(ME.into());
    old.listed = true;
    old.private = true;
    store.upsert_conversation(&account(), &old).unwrap();
    drop(store);
    world.steer("drop");
    world.until("the cached self conversation to be renamed", || {
        (world.conversation(ME)?.title == "Note to Self").then_some(())
    });
    assert_eq!(world.messages(ME).len(), 1, "renaming must preserve messages");
}

#[test]
fn a_payment_and_a_gift_badge_say_where_they_are_acting_on() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("payment grace {} for coffee", T0 + 4_000));
    let paid = world.arrived(GRACE, &id(T0 + 4_000, GRACE));
    assert_eq!(
        (paid.text.as_str(), &paid.kind),
        ("Payment · open Signal on your phone — for coffee", &MessageKind::Unsupported { link: String::new() })
    );
    // No note, no dash.
    world.steer(&format!("payment grace {}", T0 + 5_000));
    let bare = world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert_eq!(bare.text.as_str(), "Payment · open Signal on your phone");
    // Redeeming is the phone's work, and the row says so.
    world.steer(&format!("giftbadge grace {}", T0 + 6_000));
    let badge = world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    assert_eq!(badge.text.as_str(), "Gift badge · open Signal on your phone to redeem");
}

#[test]
fn styled_text_is_sent_as_ranges_and_a_spoiler_stays_out_of_the_preview() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} hello", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.send(GRACE, "5001", "**Hold** ||the plan||", None);
    let styled = world.arrived(GRACE, &id(FIRST_SENT, ME));
    assert_eq!(styled.text, "Hold the plan");
    let bold = styled.spans.iter().any(|span| span.style == protocol::Style::Bold && styled.text.get(span.start as usize..span.end as usize) == Some("Hold"));
    let hidden = styled.spans.iter().any(|span| span.style == protocol::Style::Spoiler && styled.text.get(span.start as usize..span.end as usize) == Some("the plan"));
    assert!(bold && hidden, "{:?}", styled.spans);
    world.heard(&format!("send {GRACE} text Hold the plan ranges=Bold:0+4,Spoiler:5+8"));
    let preview = world.conversation(GRACE).unwrap().preview;
    assert!(!preview.contains("plan"), "{preview}");
    world.send(GRACE, "5002", "ok", Some(&styled.id));
    let answered = world.arrived(GRACE, &id(FIRST_SENT + 1_000, ME));
    assert_eq!(answered.reply_to.as_ref().map(|reply| reply.snippet.as_str()), Some("Hold Spoiler"));
    assert_eq!(world.ask(Request::Edit { conversation: key(GRACE), message_id: styled.id, text: "~~no~~".into() }), Response::Ok);
    world.heard(&format!("send {GRACE} edit {FIRST_SENT} no ranges=Strike:0+2"));
    let edited = world.until("the strike", || {
        world.messages(GRACE).into_iter().find(|message| message.text == "no" && message.spans.iter().any(|span| span.style == protocol::Style::Strike))
    });
    assert_eq!(edited.text.get(edited.spans[0].start as usize..edited.spans[0].end as usize), Some("no"));
}

#[test]
fn a_changed_reaction_after_restart_replaces_the_old_emoji() {
    let mut world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} Lunch?", T0 + 1_000));
    let theirs = world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer(&format!("incoming grace {} Still there?", T0 + 2_000));
    let again = world.arrived(GRACE, &id(T0 + 2_000, GRACE));

    world.steer(&format!("react grace grace {} 👍", T0 + 1_000));
    world.until("Grace's first reaction", || {
        world.messages(GRACE).into_iter().find(|message| message.id == theirs.id && message.reactions.iter().any(|reaction| reaction.emoji == "👍"))
    });
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "👍".into(), on: true }), Response::Ok);
    world.until("our first reaction", || {
        world.messages(GRACE).into_iter().find(|message| message.id == again.id && message.reactions.iter().any(|reaction| reaction.emoji == "👍" && reaction.mine))
    });

    world.restart();
    world.connected();

    world.steer(&format!("react grace grace {} ❤️", T0 + 1_000));
    let replaced = world.until("Grace's new reaction", || {
        world.messages(GRACE).into_iter().find(|message| message.id == theirs.id && message.reactions.iter().any(|reaction| reaction.emoji == "❤️"))
    });
    let emojis: Vec<&str> = replaced.reactions.iter().map(|reaction| reaction.emoji.as_str()).collect();
    assert_eq!(emojis, ["❤️"], "{:?}", replaced.reactions);

    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "❤️".into(), on: true }), Response::Ok);
    let held = world.until("our new reaction", || {
        world.messages(GRACE).into_iter().find(|message| message.id == again.id && message.reactions.iter().any(|reaction| reaction.emoji == "❤️" && reaction.mine))
    });
    let mine: Vec<&str> = held.reactions.iter().map(|reaction| reaction.emoji.as_str()).collect();
    assert_eq!(mine, ["❤️"], "{:?}", held.reactions);
}

#[test]
fn a_username_lookup_opens_a_chat() {
    let world = World::linked();
    world.connected();
    let ada = "d4d4d4d4-0000-4000-8000-000000000004";
    world.steer(&format!("lookup ada.42 {ada}"));
    let search = |query: &str| Request::People {
        network: Some(Network::Signal),
        relation: None,
        query: Some(query.into()),
        offset: 0,
        limit: 20,
    };
    let Response::People { people, .. } = world.ask(search("ada.42")) else {
        panic!("no people");
    };
    assert!(people.iter().all(|person| person.handle != "ada.42"));
    let found = world.until("ada.42", || match world.ask(search("ada.42")) {
        Response::People { people, .. } => people.into_iter().find(|person| person.handle == "ada.42" && person.key.id == ada),
        _ => None,
    });
    world.heard("lookup ada.42");
    let Response::Conversation { conversation } = world.ask(Request::StartDirect { person: found.key }) else {
        panic!("no conversation");
    };
    assert_eq!((conversation.key.id.as_str(), conversation.kind, conversation.title.as_str()), (ada, ConversationKind::Direct, "ada.42"));
    let (_, seq) = world.status();
    world.ask(search("Grace"));
    world.changed(seq, "the name search to finish", |change| match change {
        Change::Searched { query, .. } if query == "Grace" => Some(()),
        _ => None,
    });
    assert!(!world.calls().iter().any(|call| call.contains("Grace")), "{:?}", world.calls());

    // Looking up a username that resolves to an already-known contact keeps their name and avatar.
    world.ask(Request::Watch(protocol::Watching { people: vec![key(GRACE)], ..Default::default() }));
    let avatar = world.until("Grace's picture", || match world.ask(search("Grace")) {
        Response::People { people, .. } => people.into_iter().find(|person| person.key.id == GRACE).and_then(|person| person.avatar),
        _ => None,
    });
    world.steer(&format!("lookup grace.42 {GRACE}"));
    let (_, seq) = world.status();
    world.ask(search("grace.42"));
    world.changed(seq, "grace.42 lookup to finish", |change| match change {
        Change::Searched { query, .. } if query == "grace.42" => Some(()),
        _ => None,
    });
    let Response::People { people, .. } = world.ask(search("grace.42")) else {
        panic!("no people");
    };
    let grace = people.into_iter().find(|person| person.key.id == GRACE).expect("Grace after username lookup");
    assert_eq!((grace.name.as_str(), grace.handle.as_str()), ("Grace Ombra", "grace.42"));
    assert_eq!(grace.avatar.as_ref().map(|file| &file.id), Some(&avatar.id));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// The phone deleted one message for this device only. It leaves the chat, with no time limit.
#[test]
fn delete_for_me_from_the_phone_removes_the_message() {
    let world = World::linked();
    world.connected();
    let sent = now_ms();
    world.steer(&format!("incoming grace {sent} Keep this?"));
    let message = world.arrived(GRACE, &id(sent, GRACE));
    world.steer(&format!("delete-for-me grace {sent}"));
    world.until("the message to leave", || (!world.messages(GRACE).iter().any(|m| m.id == message.id)).then_some(()));
    assert!(world.conversation(GRACE).is_some(), "deleting one message keeps the chat");
}

/// A delete for everyone older than Signal's receive window stays. A fresh one still lands.
#[test]
fn a_late_delete_for_everyone_is_ignored() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {T0} Old"));
    let old = world.arrived(GRACE, &id(T0, GRACE));
    let fresh = now_ms();
    world.steer(&format!("incoming grace {fresh} New"));
    world.arrived(GRACE, &id(fresh, GRACE));
    world.steer(&format!("delete grace {T0}"));
    world.steer(&format!("delete grace {fresh}"));
    world.until("the fresh delete", || (!world.messages(GRACE).iter().any(|m| m.id == id(fresh, GRACE))).then_some(()));
    assert!(world.messages(GRACE).iter().any(|m| m.id == old.id), "a two-day-old delete for everyone is ignored");
}

/// Past the send window, Delete removes the message here and tells the phone, and does not tell everyone.
#[test]
fn deleting_an_old_message_is_delete_for_me() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("mine {T0} From the phone"));
    let old = world.arrived(GRACE, &id(T0, ME));
    let refused = world.ask(Request::Edit { conversation: key(GRACE), message_id: old.id.clone(), text: "Too late".into() });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail.contains("no longer")), "{refused:?}");
    assert!(!world.calls().iter().any(|call| call.contains(&format!("edit {T0}"))));
    assert_eq!(world.ask(Request::Delete { conversation: key(GRACE), message_id: old.id.clone() }), Response::Ok);
    world.heard(&format!("send {GRACE} delete-for-me {T0}"));
    assert!(!world.calls().iter().any(|call| call == &format!("send {GRACE} delete {T0}")));
    world.until("the local delete", || (!world.messages(GRACE).iter().any(|m| m.id == old.id)).then_some(()));
}

/// The phone deleted the whole chat for this device.
#[test]
fn delete_chat_for_me_from_the_phone_removes_the_chat() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} hello", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer("delete-chat grace");
    world.until("the chat to leave the list", || {
        let row = world.conversation(GRACE)?;
        (row.hidden && world.messages(GRACE).is_empty()).then_some(())
    });
}

/// Signal's edit window is one day. There is no delete-window field to report beside it.
#[test]
fn signal_reports_a_one_day_edit_window() {
    let world = World::linked();
    world.connected();
    let (accounts, _) = world.status();
    let signal = accounts.into_iter().find(|status| status.key == account()).unwrap();
    assert_eq!(signal.can.edit_window, 24 * 60 * 60);
}

/// A day-old message is not offered as a delete for everyone. Delete for me stays.
#[test]
fn a_delete_older_than_the_window_is_not_offered_for_everyone() {
    let hour = 60 * 60 * 1000;
    let now = 50 * hour;
    let recent = chatd::adapters::signal::can(now - hour, now, true, false);
    assert!(recent.edit && recent.delete_for_everyone && recent.delete_for_me);
    let late = chatd::adapters::signal::can(now - 25 * hour, now, true, false);
    assert!(!late.edit && !late.delete_for_everyone && late.delete_for_me, "{late:?}");
    let note = chatd::adapters::signal::can(now - 25 * hour, now, true, true);
    assert!(note.delete_for_everyone && !note.edit && note.delete_for_me);
    let theirs = chatd::adapters::signal::can(now - hour, now, false, false);
    assert!(!theirs.edit && !theirs.delete_for_everyone && theirs.delete_for_me);
    assert!(chatd::adapters::signal::accept_remote(now - 47 * hour, now, false));
    assert!(!chatd::adapters::signal::accept_remote(now - 49 * hour, now, false));
    assert!(chatd::adapters::signal::accept_remote(now - 49 * hour, now, true));
}

/// A rename while connected updates the row and writes one system line. The first snapshot does not.
#[test]
fn a_renamed_group_takes_the_new_name_and_one_system_row() {
    let world = World::linked();
    world.connected();
    assert_eq!(world.conversation(CLUB).unwrap().title, "Bridge club");
    assert!(world.messages(CLUB).iter().all(|message| message.kind != MessageKind::System));
    world.steer("group-title Wednesday club");
    world.until("the new name", || (world.conversation(CLUB)?.title == "Wednesday club").then_some(()));
    let systems: Vec<_> = world.messages(CLUB).into_iter().filter(|message| message.kind == MessageKind::System).collect();
    assert_eq!(systems.len(), 1, "{systems:?}");
    assert_eq!(systems[0].text, "The group name is now Wednesday club");
    assert!(!world.conversation(CLUB).unwrap().read_only);

    // Leaving marks the group read-only, a rename while removed keeps it read-only,
    // and being re-added clears read-only again.
    world.steer("group-leave");
    world.until("read-only after leave", || world.conversation(CLUB)?.read_only.then_some(()));
    world.steer("group-title Thursday club");
    world.until("renamed while removed", || (world.conversation(CLUB)?.title == "Thursday club").then_some(()));
    assert!(world.conversation(CLUB).unwrap().read_only);
    world.steer("group-rejoin");
    world.until("writable after rejoin", || (!world.conversation(CLUB)?.read_only).then_some(()));
}

/// The sentence for a rename, a join, and leaving. The connect snapshot is not a sentence.
#[test]
fn a_group_delta_names_the_rename_the_join_and_the_leave() {
    chatd::adapters::signal::prove_signal_group_rename().expect("rename");
    let before = chatd::adapters::signal::GroupSnap {
        members: vec![chatd::adapters::signal::MemberSnap { id: ME.into(), admin: false }],
        ..chatd::adapters::signal::GroupSnap::default()
    };
    let mut members = before.members.clone();
    members.push(chatd::adapters::signal::MemberSnap { id: GRACE.into(), admin: false });
    let after = chatd::adapters::signal::GroupSnap { members, ..chatd::adapters::signal::GroupSnap::default() };
    let joined = chatd::adapters::signal::group_delta(&before, &after, ME, |id| if id == GRACE { "Grace Ombra".into() } else { String::new() });
    assert_eq!(joined.lines, ["Grace Ombra joined the group"]);
    assert!(!joined.left);
}

#[test]
fn a_group_poll_shows_its_counts_and_a_vote_reaches_the_wire() {
    let world = World::linked();
    world.connected();
    let sent = T0 + 4_000;
    world.steer(&format!("poll grace {sent} Dinner? | Thai | Pizza"));
    let poll = world.arrived(CLUB, &id(sent, GRACE));
    let held = poll.poll.expect("a poll");
    assert_eq!(poll.text, "Poll: Dinner?");
    assert_eq!(poll.kind, MessageKind::Normal);
    assert_eq!(held.question, "Dinner?");
    assert!(!held.multiple && !held.closed && held.mine.is_empty());
    assert_eq!(held.choices.iter().map(|choice| (choice.text.as_str(), choice.count)).collect::<Vec<_>>(), [("Thai", 0), ("Pizza", 0)]);

    // Grace moves from Thai to Pizza; Alan picks Thai. Counts follow the latest vote.
    world.steer(&format!("vote grace grace {sent} 0"));
    world.steer(&format!("vote grace grace {sent} 1"));
    world.steer(&format!("vote alan grace {sent} 0"));
    let voted = world.until("both votes", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == poll.id)?;
        let poll = message.poll.as_ref()?;
        (poll.choices[0].count == 1 && poll.choices[1].count == 1).then_some(message)
    });
    let voted = voted.poll.unwrap();
    assert_eq!(voted.choices[0].voters, vec![ALAN.to_owned()]);
    assert_eq!(voted.choices[1].voters, vec![GRACE.to_owned()]);

    let multiple_at = T0 + 8_000;
    world.steer(&format!("pollmulti alan {multiple_at} Sides? | Soup | Salad"));
    world.steer(&format!("vote alan alan {multiple_at} 0,1"));
    world.until("both sides", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == id(multiple_at, ALAN))?;
        let poll = message.poll.as_ref()?;
        (poll.multiple && poll.choices.iter().all(|choice| choice.count == 1)).then_some(())
    });

    // A file cannot ride along. Nothing is asked of Signal.
    let with_file = Request::Send {
        conversation: key(CLUB),
        client_id: "poll-file".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            poll: Some(PollDraft { question: "Dinner?".into(), options: vec!["Thai".into(), "Pizza".into()], multiple: false }),
            files: vec![OutgoingFile { path: "missing.txt".into(), name: String::new(), spoiler: false, as_file: false, original: false }],
            ..Outgoing::default()
        },
    };
    match world.ask(with_file) {
        Response::Error { code: ErrorCode::Refused, detail } => assert_eq!(detail, "A poll can't carry a file."),
        other => panic!("{other:?}"),
    }
    assert!(!world.calls().iter().any(|call| call.contains("poll")));

    // Our vote is on the wire, then a retraction. Ending someone else's poll is refused.
    assert_eq!(world.ask(Request::Poll { conversation: key(CLUB), message_id: poll.id.clone(), votes: Some(vec![0]) }), Response::Ok);
    world.heard(&format!("send {CLUB} vote {sent}-{GRACE} 0 serial=1"));
    world.until("our vote", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == poll.id)?;
        let held = message.poll.as_ref()?;
        (held.mine == vec![0] && held.choices[0].count == 2 && message.text == "Poll: Dinner?").then_some(())
    });
    assert_eq!(world.ask(Request::Poll { conversation: key(CLUB), message_id: poll.id.clone(), votes: Some(vec![]) }), Response::Ok);
    world.heard(&format!("send {CLUB} vote {sent}-{GRACE} - serial=2"));
    world.until("the retraction", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == poll.id)?;
        let held = message.poll?;
        (held.mine.is_empty() && held.choices[0].count == 1 && held.choices[1].count == 1).then_some(())
    });
    match world.ask(Request::Poll { conversation: key(CLUB), message_id: poll.id.clone(), votes: None }) {
        Response::Error { code: ErrorCode::Refused, detail } => assert!(detail.contains("started"), "{detail}"),
        other => panic!("{other:?}"),
    }

    let started = Request::Send {
        conversation: key(CLUB),
        client_id: "poll-1".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            poll: Some(PollDraft { question: "Next week?".into(), options: vec!["Yes".into(), "No".into()], multiple: false }),
            ..Outgoing::default()
        },
    };
    let Response::Sent { message: pending } = world.ask(started) else { panic!("not sent") };
    assert_eq!(pending.text, "Poll: Next week?");
    // The vote and the retraction were the first two sends, so this poll is the third.
    let created = FIRST_SENT + 2_000;
    let ours = world.arrived(CLUB, &id(created, ME));
    assert_eq!(ours.client_id.as_deref(), Some("poll-1"));
    assert_eq!(ours.poll.as_ref().map(|poll| poll.choices.len()), Some(2));
    world.heard(&format!("send {CLUB} poll Next week? | Yes | No multi=false"));
    assert_eq!(world.ask(Request::Poll { conversation: key(CLUB), message_id: ours.id.clone(), votes: None }), Response::Ok);
    world.heard(&format!("send {CLUB} endpoll {created}"));
    world.until("our poll to end", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == ours.id)?;
        message.poll.as_ref().filter(|poll| poll.closed).map(|_| ())
    });
    world.steer(&format!("endpoll grace {sent}"));
    world.until("Grace's poll to end", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == poll.id)?;
        message.poll.as_ref().filter(|poll| poll.closed).map(|_| ())
    });
}
