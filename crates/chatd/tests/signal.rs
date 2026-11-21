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
        World::start_with_setup(session, |_| {})
    }

    fn start_with_setup(session: Option<&str>, setup: impl FnOnce(&std::path::Path)) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-sg-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        setup(&dir);
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
    world.steer(&format!("giftbadge grace {}", T0 + 3_000));
    let badge = world.arrived(GRACE, &id(T0 + 3_000, GRACE));
    assert_eq!(
        (badge.text.as_str(), &badge.kind),
        ("Gift badge · open Signal on your phone to redeem", &MessageKind::Unsupported { link: String::new() })
    );
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
    // A normal photo without the view-once flag arrives as a real image attachment with a BlurHash preview.
    world.steer(&format!("photo grace {}", T0 + 6_000));
    let photo = world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    assert_eq!(photo.kind, MessageKind::Normal);
    assert_eq!(photo.attachments.len(), 1);
    assert_eq!(photo.attachments[0].file.kind, protocol::MediaKind::Image);
    assert!(photo.attachments[0].preview.is_some(), "photo should carry an instant BlurHash preview");
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
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "👋".into(), on: true , burst: false }), Response::Ok);
    world.heard(&format!("send {GRACE} react {}-{GRACE} 👋 remove=false", T0 + 2_000));
    // One reaction a person: another replaces mine at once and on Signal, and is the only one.
    let Response::Reactions { rules, .. } = world.ask(Request::Reactions { conversation: key(GRACE) }) else {
        panic!("no rules");
    };
    assert_eq!((rules.mine, rules.replaces, rules.custom), (1, true, protocol::EmojiScope::None));
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "🎉".into(), on: true , burst: false }), Response::Ok);
    let held = world.messages(GRACE).into_iter().find(|m| m.id == again.id).unwrap();
    let mine: Vec<String> = held.reactions.into_iter().filter(|r| r.mine).map(|r| r.emoji).collect();
    assert_eq!(mine, ["🎉"]);
    world.heard(&format!("send {GRACE} react {}-{GRACE} 🎉 remove=false", T0 + 2_000));
    // A custom emoji is nothing Signal can carry, so it is never sent.
    let custom = world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: ":party:".into(), on: true , burst: false });
    assert!(matches!(&custom, Response::Error { code: protocol::ErrorCode::Refused, .. }), "{custom:?}");
    assert!(!world.calls().iter().any(|call| call.contains(":party:")));
    assert_eq!(
        world.ask(Request::Edit { conversation: key(GRACE), message_id: ours.id.clone(), text: "Sure!".into(), spans: Vec::new() }),
        Response::Ok
    );

    world.heard(&format!("send {GRACE} edit {ours_at} Sure!"));
    assert!(world.messages(GRACE).iter().any(|m| m.id == ours.id && m.edited && m.text == "Sure!"));
    let refused = world.ask(Request::Edit { conversation: key(GRACE), message_id: again.id.clone(), text: "Not mine".into(), spans: Vec::new() });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail.contains("own messages")), "{refused:?}");
    // Grace's phone has ours, then Grace read it.
    assert!(!world.conversation(GRACE).unwrap().delivered);
    world.steer(&format!("delivered grace {ours_at}"));
    world.until("delivered", || world.conversation(GRACE)?.delivered.then_some(()));
    world.steer(&format!("seen grace {ours_at}"));
    world.until("seen", || world.conversation(GRACE)?.seen.then_some(()));
    assert_eq!(world.ask(Request::Delete { conversation: key(GRACE), message_id: ours.id.clone(), scope: protocol::DeleteScope::Everyone }), Response::Ok);
    world.heard(&format!("send {GRACE} delete {ours_at}"));
}

#[test]
fn a_group_message_names_who_read_it_and_stays_unseen_until_both_have() {
    let world = World::linked();
    world.connected();
    world.send(CLUB, "7301", "Deal?", None);
    let ours = world.until("our group message", || {
        world.messages(CLUB).into_iter().find(|message| message.client_id.as_deref() == Some("7301") && message.id != "7301")
    });
    let ours_at = ours.id.split_once('-').unwrap().0;
    assert!(ours.deliveries.is_empty(), "{:?}", ours.deliveries);

    world.steer(&format!("delivered grace {ours_at}"));
    let grace = world.until("Grace's delivery", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == ours.id)?;
        (message.deliveries.len() == 1).then_some(message)
    });
    assert_eq!(grace.deliveries[0].person, GRACE);
    assert!(grace.deliveries[0].delivered.is_some());
    assert!(!world.conversation(CLUB).unwrap().delivered, "one phone is not the whole group");
    assert!(!world.conversation(CLUB).unwrap().seen);

    world.steer(&format!("delivered alan {ours_at}"));
    world.until("both deliveries", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == ours.id)?;
        (message.deliveries.len() == 2).then_some(())
    });
    world.until("delivered once everyone has it", || world.conversation(CLUB)?.delivered.then_some(()));
    assert!(!world.conversation(CLUB).unwrap().seen);

    world.steer(&format!("seen grace {ours_at}"));
    world.until("Grace's read", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == ours.id)?;
        message.deliveries.iter().any(|delivery| delivery.person == GRACE && delivery.read.is_some()).then_some(())
    });
    assert!(!world.conversation(CLUB).unwrap().seen, "one reader is not the whole group");

    world.steer(&format!("viewed alan {ours_at}"));
    let both = world.until("both reads", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == ours.id)?;
        let read = message.deliveries.iter().filter(|delivery| delivery.read.is_some()).count();
        (read == 2).then_some(message)
    });
    let alan = both.deliveries.iter().find(|delivery| delivery.person == ALAN).expect("Alan");
    assert!(alan.viewed.is_some() && alan.read.is_some());
    world.until("seen once everyone has read", || world.conversation(CLUB)?.seen.then_some(()));
}

#[test]
fn a_receipt_after_restart_stays_on_the_group_message() {
    let mut world = World::linked();
    world.connected();
    world.send(CLUB, "7302", "Tuesday", None);
    let ours = world.until("our group message", || {
        world.messages(CLUB).into_iter().find(|message| message.client_id.as_deref() == Some("7302") && message.id != "7302")
    });
    let message_id = ours.id.clone();
    let ours_at = ours.id.split_once('-').unwrap().0.to_owned();
    world.restart();
    world.connected();
    world.steer(&format!("delivered grace {ours_at}"));
    let message = world.until("the receipt on the group message", || {
        let message = world.messages(CLUB).into_iter().find(|message| message.id == message_id)?;
        message.deliveries.iter().any(|delivery| delivery.person == GRACE).then_some(message)
    });
    assert_eq!(message.deliveries.len(), 1);
    assert!(world.messages(GRACE).iter().all(|message| message.id != message_id), "a group receipt was filed on the reader's direct chat");
}

#[test]
fn a_message_says_when_read_receipts_are_off_and_names_who_a_send_missed() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} hi", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer("receipts off");
    assert_eq!(world.ask(Request::MarkRead { conversation: key(GRACE), message_id: id(T0 + 1_000, GRACE) }), Response::Ok);
    world.heard(&format!("read receipts=false {}", id(T0 + 1_000, GRACE)));
    world.steer("fail grace");
    world.send(CLUB, "7303", "Quiet", None);
    let ours = world.until("our group message", || {
        world.messages(CLUB).into_iter().find(|message| message.client_id.as_deref() == Some("7303") && message.id != "7303")
    });
    assert_eq!(ours.receipt_note, "Read receipts are off");
    let missed = ours.deliveries.iter().find(|delivery| delivery.person == GRACE).expect("Grace was missed");
    assert_eq!(missed.failed, "Signal could not deliver this.");
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
        world.ask(Request::Typing { conversation: key(GRACE), activity: protocol::Compose::Typing });
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
    world.ask(Request::Typing { conversation: key(GRACE), activity: protocol::Compose::Typing });
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
    world.ask(Request::Typing { conversation: key(ALAN), activity: protocol::Compose::Typing });
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
                view_once: false, media: None,
            }],
            ..protocol::Outgoing::default()
        },
    };
    let pending = match world.ask(asked) {
        Response::Sent { message } => message,
        other => panic!("{other:?}"),
    };
    assert!(matches!(pending.state, SendState::Pending), "{pending:?}");
    let sent = world.until("the sent file", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("5001") && message.id != "5001")
    });
    let sent_at = sent.id.split_once('-').unwrap().0;
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
    world.steer(&format!("mine {sent_at} from the phone"));
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
            pronouns: None,
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
        pronouns: None,
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
        pronouns: None,
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
    let styled = world.until("styled message", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("5001") && message.id != "5001")
    });
    let styled_at = styled.id.split_once('-').unwrap().0.to_owned();
    assert_eq!(styled.text, "Hold the plan");
    let bold = styled.spans.iter().any(|span| span.style == protocol::Style::Bold && styled.text.get(span.start as usize..span.end as usize) == Some("Hold"));
    let hidden = styled.spans.iter().any(|span| span.style == protocol::Style::Spoiler && styled.text.get(span.start as usize..span.end as usize) == Some("the plan"));
    assert!(bold && hidden, "{:?}", styled.spans);
    world.heard(&format!("send {GRACE} text Hold the plan ranges=Bold:0+4,Spoiler:5+8"));
    let preview = world.conversation(GRACE).unwrap().preview;
    assert!(!preview.contains("plan"), "{preview}");
    world.send(GRACE, "5002", "ok", Some(&styled.id));
    let answered = world.until("reply to styled", || {
        world.messages(GRACE).into_iter().find(|message| message.client_id.as_deref() == Some("5002") && message.id != "5002")
    });
    assert_eq!(answered.reply_to.as_ref().map(|reply| reply.snippet.as_str()), Some("Hold Spoiler"));
    assert_eq!(world.ask(Request::Edit { conversation: key(GRACE), message_id: styled.id, text: "~~no~~".into(), spans: Vec::new() }), Response::Ok);
    world.heard(&format!("send {GRACE} edit {styled_at} no ranges=Strike:0+2"));
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
    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "👍".into(), on: true , burst: false }), Response::Ok);
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

    assert_eq!(world.ask(Request::React { conversation: key(GRACE), message_id: again.id.clone(), emoji: "❤️".into(), on: true , burst: false }), Response::Ok);
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
    let refused = world.ask(Request::Edit { conversation: key(GRACE), message_id: old.id.clone(), text: "Too late".into(), spans: Vec::new() });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail.contains("no longer")), "{refused:?}");
    assert!(!world.calls().iter().any(|call| call.contains(&format!("edit {T0}"))));
    assert_eq!(world.ask(Request::Delete { conversation: key(GRACE), message_id: old.id.clone(), scope: protocol::DeleteScope::Everyone }), Response::Ok);
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
            poll: Some(PollDraft { question: "Dinner?".into(), options: vec!["Thai".into(), "Pizza".into()], multiple: false, ..PollDraft::default() }),
            files: vec![OutgoingFile { path: "missing.txt".into(), name: String::new(), spoiler: false, as_file: false, original: false, view_once: false, media: None }],
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
            poll: Some(PollDraft { question: "Next week?".into(), options: vec!["Yes".into(), "No".into()], multiple: false, ..PollDraft::default() }),
            ..Outgoing::default()
        },
    };
    let Response::Sent { message: pending } = world.ask(started) else { panic!("not sent") };
    assert_eq!(pending.text, "Poll: Next week?");
    let ours = world.until("our poll", || {
        world.messages(CLUB).into_iter().find(|message| message.client_id.as_deref() == Some("poll-1") && message.id != "poll-1")
    });
    let created = ours.id.split_once('-').unwrap().0;
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

#[test]
fn disappearing_messages_carry_timer_and_expire_after_read() {
    let world = World::linked();
    world.connected();

    let sent = T0 + 10_000;
    world.steer(&format!("expiring grace {sent} 2 Secret note"));
    let arrived = world.arrived(GRACE, &id(sent, GRACE));
    assert_eq!(arrived.text, "Secret note");
    // Not read yet, so it has no expiry timestamp yet.
    assert!(arrived.expires_at.is_none());

    // Sending a reply in this conversation carries the conversation's active expire_timer = 2.
    let _ = world.send(GRACE, "exp-1", "Got it", None);
    let reply = world.until("settled outgoing disappearing message", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("exp-1") && m.id != "exp-1")
    });
    assert!(reply.expires_at.is_some(), "outgoing disappearing message must have expires_at");
    world.heard(&format!("send {GRACE} text Got it expire=2"));

    // Reading Grace's message starts its 2-second countdown and removes it from the store.
    assert_eq!(
        world.ask(Request::MarkRead {
            conversation: key(GRACE),
            message_id: arrived.id.clone(),
        }),
        Response::Ok
    );
    world.until("expired message to be deleted from store", || {
        let still_there = world.messages(GRACE).into_iter().any(|m| m.id == arrived.id);
        (!still_there).then_some(())
    });

    // Timer updates produce a system row and clear or update the outgoing timer.
    world.steer("timer grace 60");
    world.until("timer set system row", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.kind == MessageKind::System && m.text == "Disappearing messages are set to 1 minute")
    });
    let _ = world.send(GRACE, "exp-2", "Still disappearing", None);
    world.heard(&format!("send {GRACE} text Still disappearing expire=60"));

    world.steer("timer grace 0");
    world.until("timer off system row", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.kind == MessageKind::System && m.text == "Disappearing messages are off")
    });
    let _ = world.send(GRACE, "exp-3", "Permanent now", None);
    world.heard(&format!("send {GRACE} text Permanent now"));
}

#[test]
fn sending_with_a_mention_span_sends_a_signal_mention_range() {
    let world = World::linked();
    world.connected();

    let text = "hi @Grace Ombra!";
    let start = text.find("@Grace Ombra").unwrap() as u32;
    let end = start + "@Grace Ombra".len() as u32;
    let asked = Request::Send {
        conversation: key(CLUB),
        client_id: "mention-1".into(),
        text: text.into(),
        reply_to: None,
        with: Outgoing {
            spans: vec![protocol::Span {
                start,
                end,
                style: protocol::Style::Mention {
                    kind: protocol::MentionKind::User,
                    id: GRACE.into(),
                },
            }],
            ..Outgoing::default()
        },
    };
    let Response::Sent { .. } = world.ask(asked) else {
        panic!("not sent");
    };
    let settled = world.until("settled mention message", || {
        world
            .messages(CLUB)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("mention-1") && m.id != "mention-1")
    });
    assert_eq!(settled.text, text);
    assert!(
        settled.spans.iter().any(|s| matches!(
            &s.style,
            protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == GRACE
        )),
        "stored message lost its mention span: {:?}",
        settled.spans
    );
    world.heard(&format!("send {CLUB} text hi \u{FFFC}! ranges=Mention({GRACE}):3+1"));
}

#[test]
fn mute_archive_pin_and_mark_unread_sync_and_serve_commands() {
    let world = World::linked();
    world.connected();

    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|status| status.key == account()).expect("Signal account");
    assert!(signal.can.pin_chats > 0, "Signal should advertise pin_chats");
    assert!(signal.can.archive, "Signal should advertise archive");

    // Create direct chats with Grace and Alan.
    world.steer(&format!("incoming grace {} Hey", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer(&format!("incoming alan {} Yo", T0 + 2_000));
    world.arrived(ALAN, &id(T0 + 2_000, ALAN));

    // Sync from phone: Grace is muted, archived, and marked unread; CLUB is pinned.
    world.steer("chat-state grace muted=9999999999999 archived=true unread=true");
    world.steer("pin club");
    world.until("Grace muted and archived", || {
        let row = world.conversation(GRACE)?;
        (row.muted && row.archived && row.unread > 0).then_some(())
    });
    world.until("Club pinned", || world.conversation(CLUB)?.pin.map(|p| p == 0).filter(|&ok| ok));

    // Archive shelf contains Grace; Main shelf does not contain Grace and puts pinned CLUB first.
    let Response::Conversations { conversations: archive_rows, .. } = world.ask(Request::Home {
        network: Network::Signal,
        account: Some(account()),
        folder: None,
        shelf: protocol::Shelf::Archive,
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Archive conversations");
    };
    assert!(archive_rows.iter().any(|c| c.key.id == GRACE), "Grace should be on Archive shelf");

    let Response::Conversations { conversations: main_rows, .. } = world.ask(Request::Home {
        network: Network::Signal,
        account: Some(account()),
        folder: None,
        shelf: protocol::Shelf::Main,
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Main conversations");
    };
    assert!(!main_rows.iter().any(|c| c.key.id == GRACE), "Grace should not be on Main shelf");
    assert_eq!(main_rows.first().map(|c| c.key.id.as_str()), Some(CLUB), "Pinned CLUB should sort first");

    // Keep muted chats archived: with setting ON, a new message from muted Grace keeps her archived.
    world.steer("keep-muted-archived on");
    world.steer(&format!("incoming grace {} Still muted", T0 + 3_000));
    world.arrived(GRACE, &id(T0 + 3_000, GRACE));
    assert!(world.conversation(GRACE).unwrap().archived, "muted archived chat stays archived when keep_muted_archived is on");

    // With setting OFF, a new message unarchives Grace back to Main.
    world.steer("keep-muted-archived off");
    world.steer(&format!("incoming grace {} Wake up", T0 + 4_000));
    world.arrived(GRACE, &id(T0 + 4_000, GRACE));
    world.until("Grace unarchived on new message", || (!world.conversation(GRACE)?.archived).then_some(()));

    // Muted group with dont_notify_mentions suppresses mentions_me.
    world.steer("chat-state club muted=9999999999999 dont_notify_mentions=true");
    world.until("Club muted", || world.conversation(CLUB)?.muted.then_some(()));
    world.steer(&format!("mention grace {} quiet ping", T0 + 5_000));
    let quiet_mention = world.arrived(CLUB, &id(T0 + 5_000, GRACE));
    assert!(!quiet_mention.mentions_me, "muted group with dontNotifyForMentionsIfMuted should suppress mentions_me");

    // Serve SetMute, File (archive + pin), and MarkUnread from tinyclient.
    assert_eq!(
        world.ask(Request::SetMute { conversation: key(ALAN), notify: protocol::Notify::Nothing, until: None }),
        Response::Ok
    );
    world.until("Alan muted", || world.conversation(ALAN)?.muted.then_some(()));
    assert!(world.calls().iter().any(|c| c.starts_with(&format!("mute {ALAN} "))));

    assert_eq!(
        world.ask(Request::SetMute { conversation: key(ALAN), notify: protocol::Notify::All, until: None }),
        Response::Ok
    );
    world.until("Alan unmuted", || (!world.conversation(ALAN)?.muted).then_some(()));
    world.heard(&format!("mute {ALAN} 0"));

    assert_eq!(
        world.ask(Request::File {
            conversation: key(ALAN),
            pinned: Some(true),
            archived: Some(true),
            folder_add: None,
            folder_remove: None,
        }),
        Response::Ok
    );
    world.until("Alan archived and pinned", || {
        let row = world.conversation(ALAN)?;
        (row.archived && row.pin.is_some()).then_some(())
    });
    world.heard(&format!("archive {ALAN} true"));
    world.heard(&format!("pin {ALAN} true"));

    assert_eq!(world.ask(Request::MarkRead { conversation: key(ALAN), message_id: id(T0 + 2_000, ALAN) }), Response::Ok);
    assert_eq!(world.ask(Request::MarkUnread { conversation: key(ALAN), message_id: String::new() }), Response::Ok);
    world.until("Alan marked unread", || (world.conversation(ALAN)?.unread > 0).then_some(()));
    world.heard(&format!("mark-unread {ALAN} true"));
}

#[test]
fn chat_folders_from_phone_filter_chats_and_react_to_mute_and_unread() {
    let world = World::linked();
    world.connected();

    world.steer(&format!("incoming grace {} Dinner?", T0 + 1_000));
    world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.steer(&format!("incoming alan {} Cards?", T0 + 2_000));
    world.arrived(ALAN, &id(T0 + 2_000, ALAN));

    // Sync two folders from the phone: Family (Grace + Club) and Unread (all non-muted unread, excluding Alan).
    world.steer("folders fam:Family:include=grace+club ; unr:Unread:all_dm=1,all_group=1,only_unread=1,show_muted=0,exclude=alan");

    let folders = world.until("Signal folders", || match world.ask(Request::Folders { network: Network::Signal }) {
        Response::Folders { folders } if folders.len() == 2 => Some(folders),
        _ => None,
    });
    assert_eq!(folders[0].name, "Family");
    assert_eq!(folders[1].name, "Unread");

    // Family contains Grace and Bridge club, not Alan.
    let fam_rows = world.until("Family folder rows", || {
        let Response::Conversations { conversations, .. } = world.ask(Request::Home {
            network: Network::Signal,
            account: Some(account()),
            folder: Some("fam".into()),
            shelf: protocol::Shelf::Main,
            offset: 0,
            limit: 50,
        }) else {
            return None;
        };
        (conversations.len() == 2).then_some(conversations)
    });
    let fam_ids: Vec<&str> = fam_rows.iter().map(|c| c.key.id.as_str()).collect();
    assert!(fam_ids.contains(&GRACE) && fam_ids.contains(&CLUB) && !fam_ids.contains(&ALAN));

    // Unread contains Grace (unread, unmuted), not Alan (excluded), not Club (0 unread).
    let unread_ids = || -> Vec<String> {
        let Response::Conversations { conversations, .. } = world.ask(Request::Home {
            network: Network::Signal,
            account: Some(account()),
            folder: Some("unr".into()),
            shelf: protocol::Shelf::Main,
            offset: 0,
            limit: 50,
        }) else {
            return Vec::new();
        };
        conversations.into_iter().map(|c| c.key.id).collect()
    };
    world.until("Grace in Unread folder", || (unread_ids() == vec![GRACE.to_owned()]).then_some(()));

    // Muting Grace removes her from Unread (show_muted=0).
    assert_eq!(
        world.ask(Request::SetMute { conversation: key(GRACE), notify: protocol::Notify::Nothing, until: None }),
        Response::Ok
    );
    world.until("Grace leaves Unread when muted", || unread_ids().is_empty().then_some(()));

    // Unmuting brings her back; marking her read removes her again.
    assert_eq!(
        world.ask(Request::SetMute { conversation: key(GRACE), notify: protocol::Notify::All, until: None }),
        Response::Ok
    );
    world.until("Grace returns to Unread when unmuted", || (unread_ids() == vec![GRACE.to_owned()]).then_some(()));

    assert_eq!(world.ask(Request::MarkRead { conversation: key(GRACE), message_id: id(T0 + 1_000, GRACE) }), Response::Ok);
    world.until("Grace leaves Unread when read", || unread_ids().is_empty().then_some(()));
}

#[test]
fn message_requests_from_strangers_and_blocking_work_end_to_end() {
    const STRANGER: &str = "d4d4d4d4-0000-4000-8000-000000000004";
    let world = World::linked();
    world.connected();

    // A stranger not in contacts writes first -> lands on Requests shelf with Special::Request.
    world.steer(&format!("stranger {} Who is this?", T0 + 1_000));
    world.arrived(STRANGER, &id(T0 + 1_000, STRANGER));
    let req_row = world.conversation(STRANGER).expect("stranger row");
    assert_eq!(req_row.special, protocol::Special::Request);

    let Response::Conversations { conversations: reqs, .. } = world.ask(Request::Home {
        network: Network::Signal,
        account: Some(account()),
        folder: None,
        shelf: protocol::Shelf::Requests,
        offset: 0,
        limit: 50,
    }) else {
        panic!("expected Requests shelf");
    };
    assert!(reqs.iter().any(|c| c.key.id == STRANGER), "stranger must appear on Requests shelf");

    // MarkRead and Typing on an unaccepted request send NO receipt and NO typing.
    assert_eq!(world.ask(Request::MarkRead { conversation: key(STRANGER), message_id: id(T0 + 1_000, STRANGER) }), Response::Ok);
    world.ask(Request::Typing { conversation: key(STRANGER), activity: protocol::Compose::Typing });
    sleep(Duration::from_millis(300));
    assert!(world.calls().iter().all(|c| !c.starts_with("read ") && !c.starts_with("typing ")), "no read or typing should go to an unaccepted stranger");

    // Accept moves the stranger to the main list and sends the response sync.
    assert_eq!(world.ask(Request::AnswerMessageRequest { conversation: key(STRANGER), accept: true }), Response::Ok);
    world.heard(&format!("request-response {STRANGER} accept"));
    world.until("stranger promoted to normal chat", || {
        (world.conversation(STRANGER)?.special == protocol::Special::None).then_some(())
    });

    // Block Grace via Relate: wire hears it, relation becomes Blocked, and her next message is dropped.
    assert_eq!(world.ask(Request::Relate { person: key(GRACE), action: protocol::RelateAction::Block }), Response::Ok);
    world.heard(&format!("block {GRACE}"));
    world.until("Grace marked Blocked", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Signal),
            relation: Some(Relation::Blocked),
            query: None,
            offset: 0,
            limit: 50,
        }) else {
            return None;
        };
        people.iter().any(|p| p.key.id == GRACE).then_some(())
    });

    world.steer(&format!("incoming grace {} Blocked msg", T0 + 2_000));
    sleep(Duration::from_millis(300));
    assert!(world.messages(GRACE).is_empty(), "messages from blocked sender must be dropped");

    // Unblock Grace via Relate: wire hears it and her next message arrives.
    assert_eq!(world.ask(Request::Relate { person: key(GRACE), action: protocol::RelateAction::Unblock }), Response::Ok);
    world.heard(&format!("unblock {GRACE}"));
    world.steer(&format!("incoming grace {} Back now", T0 + 3_000));
    world.arrived(GRACE, &id(T0 + 3_000, GRACE));

    // Phone syncs block list: blocking Alan from the phone drops his message.
    world.steer("blocked alan");
    world.until("Alan blocked from phone", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Signal),
            relation: Some(Relation::Blocked),
            query: None,
            offset: 0,
            limit: 50,
        }) else {
            return None;
        };
        people.iter().any(|p| p.key.id == ALAN).then_some(())
    });
    world.steer(&format!("incoming alan {} Should drop", T0 + 4_000));
    sleep(Duration::from_millis(300));
    assert!(world.messages(ALAN).is_empty(), "messages from phone-blocked sender must be dropped");
}

#[test]
fn shared_contact_arrives_as_structured_card_and_can_be_sent() {
    let world = World::linked();
    world.connected();

    // Incoming shared contact arrives as a Normal message with structured SharedContact fields, not "Contact" Unsupported.
    world.steer(&format!("contact grace {} Ada Lovelace | +44 7700 900142 | ada@example.org | Analytical Engine", T0 + 1_000));
    let msg = world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    assert_eq!(msg.kind, MessageKind::Normal, "shared contact must not be Unsupported");
    assert_eq!(msg.contacts.len(), 1);
    let card = &msg.contacts[0];
    assert_eq!(card.name, "Ada Lovelace");
    assert_eq!(card.phones, vec!["+44 7700 900142"]);
    assert_eq!(card.emails, vec!["ada@example.org"]);
    assert_eq!(card.organization, "Analytical Engine");
    assert_eq!(world.conversation(GRACE).unwrap().preview, "Contact: Ada Lovelace");

    // Sending a shared contact card reaches the wire and stores the card.
    let shared = protocol::SharedContact {
        name: "Alan Turing".into(),
        phones: vec!["+44 7700 900199".into()],
        emails: vec!["alan@bletchley.example".into()],
        organization: String::new(),
        avatar: None,
    };
    let asked = Request::Send {
        conversation: key(GRACE),
        client_id: "contact-1".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing { contact: Some(shared.clone()), ..Outgoing::default() },
    };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    world.heard(&format!("send {GRACE} contact Alan Turing"));
    let sent = world.until("sent contact card", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("contact-1") && m.id != "contact-1")
    });
    assert_eq!(sent.contacts, vec![shared]);
}

#[test]
fn creating_a_new_signal_group_reaches_the_wire_and_creates_the_row() {
    let world = World::linked();
    world.connected();

    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|status| status.key == account()).expect("Signal account");
    assert!(signal.can.groups > 0, "Signal should advertise group creation capability");

    let Response::Conversation { conversation: group } = world.ask(Request::StartGroup {
        account: account(),
        people: vec![key(GRACE), key(ALAN)],
        title: Some("Friday Bridge".into()),
    }) else {
        panic!("expected Conversation from StartGroup");
    };
    assert_eq!(group.kind, ConversationKind::Group);
    assert_eq!(group.title, "Friday Bridge");
    assert!(group.key.id.starts_with("g."), "group id should use g. prefix: {}", group.key.id);
    world.heard(&format!("create-group Friday Bridge | {GRACE},{ALAN}"));

    // The new group is stored and can be messaged.
    let stored = world.conversation(&group.key.id).expect("new group should be stored");
    assert_eq!(stored.title, "Friday Bridge");
    world.send(&group.key.id, "grp-1", "Welcome everyone!", None);
    world.heard(&format!("send {} text Welcome everyone!", group.key.id));
}

#[test]
fn leaving_a_signal_group_checks_sole_admin_and_removes_the_row() {
    let world = World::linked();
    world.connected();

    // When we are the sole admin and others remain, Leave refuses with "Choose a new admin first."
    world.steer("group-admins me");
    sleep(Duration::from_millis(200));
    let refused = world.ask(Request::Leave { conversation: key(CLUB) });
    assert!(
        matches!(&refused, Response::Error { detail, .. } if detail.contains("Choose a new admin first")),
        "expected sole-admin refusal, got {refused:?}"
    );
    assert!(world.conversation(CLUB).is_some(), "group row must remain after refusal");

    // Once another admin exists, Leave succeeds, reaches the wire, and removes the row.
    world.steer("group-admins me grace");
    sleep(Duration::from_millis(200));
    assert_eq!(world.ask(Request::Leave { conversation: key(CLUB) }), Response::Ok);
    let hex_id = CLUB.strip_prefix("g.").unwrap();
    world.heard(&format!("leave-group {hex_id}"));
    world.until("Club row removed", || world.conversation(CLUB).is_none().then_some(()));
}

#[test]
fn a_group_lists_its_members_and_admins_and_serves_conversation_info() {
    let world = World::linked();
    world.connected();

    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: key(CLUB) }) else {
        panic!("expected ConversationInfo");
    };
    assert_eq!(info.description, "Tuesdays, 7 pm");
    assert_eq!(info.members, Some(3));

    let Response::Members { sections, members, .. } = world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) else {
        panic!("expected Members");
    };
    assert_eq!(sections.len(), 2);
    assert_eq!((sections[0].name.as_str(), sections[0].count), ("Admins", 1));
    assert_eq!((sections[1].name.as_str(), sections[1].count), ("Members", 2));
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].key.id, ME);
    assert_eq!(members[0].name, "Ada Quill");
    assert_eq!(members[0].status, "Admin");
    let non_admins: Vec<&str> = members[1..].iter().map(|p| p.key.id.as_str()).collect();
    assert!(non_admins.contains(&GRACE) && non_admins.contains(&ALAN));

    // Pending invites and join requests appear in their own sections.
    let dave = "d4d4d4d4-0000-4000-8000-000000000004";
    let eve = "e5e5e5e5-0000-4000-8000-000000000005";
    world.steer(&format!("group-pending {dave} Dave"));
    world.steer(&format!("group-requesting {eve} Eve"));
    let (sections, members) = world.until("pending and requesting members", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { sections, members, .. } if sections.len() == 4 => Some((sections, members)),
            _ => None,
        }
    });
    assert_eq!((sections[2].name.as_str(), sections[2].count), ("Pending invites", 1));
    assert_eq!((sections[3].name.as_str(), sections[3].count), ("Join requests", 1));
    assert!(members.iter().any(|p| p.key.id == dave && p.status == "Invited"));
    assert!(members.iter().any(|p| p.key.id == eve && p.status == "Requested to join"));
}

#[test]
fn adding_and_removing_group_members_respects_permissions() {
    let world = World::linked();
    world.connected();

    let club = world.conversation(CLUB).unwrap();
    assert!(club.rights.add_members, "admin can add members");
    assert!(club.rights.remove_members, "admin can remove members");
    assert!(club.rights.manage_members, "admin can manage members");

    let dave = "d4d4d4d4-0000-4000-8000-000000000004";
    assert_eq!(
        world.ask(Request::MembersAdd { conversation: key(CLUB), people: vec![dave.into()] }),
        Response::Ok
    );
    world.heard(&format!("group-add {CLUB} {dave}"));
    world.until("Dave in member list", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. } if members.iter().any(|m| m.key.id == dave) => Some(()),
            _ => None,
        }
    });

    assert_eq!(
        world.ask(Request::MembersRemove { conversation: key(CLUB), person: ALAN.into() }),
        Response::Ok
    );
    world.heard(&format!("group-remove {CLUB} {ALAN}"));
    world.until("Alan removed from member list", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. } if !members.iter().any(|m| m.key.id == ALAN) => Some(()),
            _ => None,
        }
    });

    // Promote Grace to admin, then revoke.
    assert_eq!(
        world.ask(Request::MembersRemove { conversation: key(CLUB), person: format!("\u{1}signal-admin-promote\u{1}{GRACE}") }),
        Response::Ok
    );
    world.heard(&format!("group-role {CLUB} {GRACE} admin"));
    world.until("Grace promoted to admin", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. } if members.iter().any(|m| m.key.id == GRACE && m.status == "Admin") => Some(()),
            _ => None,
        }
    });
    assert_eq!(
        world.ask(Request::MembersRemove { conversation: key(CLUB), person: format!("\u{1}signal-admin-revoke\u{1}{GRACE}") }),
        Response::Ok
    );
    world.heard(&format!("group-role {CLUB} {GRACE} member"));
    world.until("Grace demoted to member", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. } if members.iter().any(|m| m.key.id == GRACE && m.status.is_empty()) => Some(()),
            _ => None,
        }
    });

    // Demote Ada and restrict member additions to admins only: both rights turn off and requests are refused.
    world.steer("group-role me member");
    world.steer("group-access members=admin");
    world.until("rights revoked for non-admin", || {
        let row = world.conversation(CLUB)?;
        (!row.rights.add_members && !row.rights.remove_members).then_some(())
    });
    let add_err = world.ask(Request::MembersAdd { conversation: key(CLUB), people: vec![ALAN.into()] });
    assert!(matches!(add_err, Response::Error { code: ErrorCode::Refused, .. }), "expected Refused, got {add_err:?}");
    let rem_err = world.ask(Request::MembersRemove { conversation: key(CLUB), person: GRACE.into() });
    assert!(matches!(rem_err, Response::Error { code: ErrorCode::Refused, .. }), "expected Refused, got {rem_err:?}");
}

#[test]
fn an_announcement_only_group_is_read_only_for_non_admins() {
    let world = World::linked();
    world.connected();

    world.steer("group-role me member");
    world.steer("group-access announcements=on");
    let row = world.until("announcement-only read_only row", || {
        let row = world.conversation(CLUB)?;
        row.read_only.then_some(row)
    });
    assert_eq!(row.read_only_reason, "Only admins can send messages");

    // Sending is refused while read-only.
    let err = world.ask(Request::Send {
        conversation: key(CLUB),
        client_id: "ann-1".into(),
        text: "Hello?".into(),
        reply_to: None,
        with: Default::default(),
    });
    assert!(matches!(err, Response::Error { code: ErrorCode::Refused, .. }), "expected Refused, got {err:?}");

    // Promoting back to admin re-enables posting.
    world.steer("group-role me admin");
    world.until("admin can post again", || {
        let row = world.conversation(CLUB)?;
        (!row.read_only).then_some(())
    });
    let sent = world.send(CLUB, "ann-2", "Admin announcement", None);
    assert_eq!(sent.text, "Admin announcement");
}

#[test]
fn editing_group_name_description_avatar_and_permissions() {
    let world = World::linked();
    world.connected();

    let long_desc = "Bridge night details: ".to_owned() + &"a".repeat(350);
    assert_eq!(
        world.ask(Request::ConversationUpdate {
            conversation: key(CLUB),
            title: Some("Wednesday Bridge".into()),
            topic: None,
            description: Some(long_desc.clone()),
        }),
        Response::Ok
    );
    let updated = world.until("updated group title", || {
        let row = world.conversation(CLUB)?;
        (row.title == "Wednesday Bridge").then_some(row)
    });
    assert_eq!(updated.topic.chars().count(), 300, "row topic is capped at 300 chars");
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: key(CLUB) }) else {
        panic!("expected ConversationInfo");
    };
    assert_eq!(info.description, long_desc, "ConversationInfo carries the full description");

    // Change group photo.
    let img = world.dir.join("group.png");
    std::fs::write(&img, b"\x89PNG\r\n\x1a\nfake").unwrap();
    assert_eq!(
        world.ask(Request::ConversationPicture { conversation: key(CLUB), path: img.to_string_lossy().into_owned() }),
        Response::Ok
    );
    world.heard(&format!("group-avatar {CLUB} 12bytes"));

    // Change permissions (attributes=admin, announcements=on).
    assert_eq!(
        world.ask(Request::ConversationUpdate {
            conversation: key(CLUB),
            title: None,
            topic: None,
            description: Some("\u{1}signal-group-perms\u{1}attributes=admin;announcements=on".into()),
        }),
        Response::Ok
    );
    world.heard(&format!("group-update {CLUB} attributes=Administrator announcements=true"));

    // Demote Ada: editing title/description/photo/permissions is now refused.
    world.steer("group-role me member");
    world.until("edit rights revoked for non-admin", || {
        let row = world.conversation(CLUB)?;
        (!row.rights.rename && !row.rights.edit_topic && !row.rights.edit_picture).then_some(())
    });
    let edit_err = world.ask(Request::ConversationUpdate {
        conversation: key(CLUB),
        title: Some("Blocked rename".into()),
        topic: None,
        description: None,
    });
    assert!(matches!(edit_err, Response::Error { code: ErrorCode::Refused, .. }));
    let pic_err = world.ask(Request::ConversationPicture { conversation: key(CLUB), path: String::new() });
    assert!(matches!(pic_err, Response::Error { code: ErrorCode::Refused, .. }));
}

#[test]
fn a_group_invite_link_renders_a_join_card_and_can_be_managed_and_joined() {
    let world = World::linked();
    world.connected();

    // Enable the group invite link as admin and verify ConversationInfo.link exposes it.
    assert_eq!(
        world.ask(Request::ConversationUpdate {
            conversation: key(CLUB),
            title: None,
            topic: None,
            description: Some("\u{1}signal-group-link\u{1}on".into()),
        }),
        Response::Ok
    );
    world.heard(&format!("group-link {CLUB} Enabled"));
    let link = world.until("invite link populated", || {
        match world.ask(Request::ConversationInfo { conversation: key(CLUB) }) {
            Response::ConversationInfo { info } if info.link.starts_with("https://signal.group/#") => Some(info.link),
            _ => None,
        }
    });
    assert_eq!(protocol::redact(&link), "https://signal.group/#[redacted]");

    // An incoming message containing the invite link gets a join card with profile-key notice.
    world.steer(&format!("incoming grace {} Join here {link}", T0 + 25_000));
    let msg = world.arrived(GRACE, &id(T0 + 25_000, GRACE));
    let card = msg.card.expect("group invite link must render a card");
    assert_eq!(card.title, "Signal group invite");
    assert!(card.text.contains("profile key"));
    assert_eq!(card.footer, "Join group");

    // Join a group from its link.
    let Response::Conversation { conversation: joined } = world.ask(Request::StartDirect {
        person: key(&format!("\u{1}signal-join-link\u{1}{link}")),
    }) else {
        panic!("expected joined group conversation");
    };
    assert!(joined.key.id.starts_with("g."));
    world.heard("group-join-link");

    // Reset and turn off the invite link.
    assert_eq!(
        world.ask(Request::ConversationUpdate {
            conversation: key(CLUB),
            title: None,
            topic: None,
            description: Some("\u{1}signal-group-link\u{1}reset".into()),
        }),
        Response::Ok
    );
    world.heard(&format!("group-link {CLUB} Reset"));
    let reset_link = world.until("reset link differs", || {
        match world.ask(Request::ConversationInfo { conversation: key(CLUB) }) {
            Response::ConversationInfo { info } if !info.link.is_empty() && info.link != link => Some(info.link),
            _ => None,
        }
    });
    assert!(reset_link.starts_with("https://signal.group/#"));
}

#[test]
fn group_join_requests_and_pending_invites_can_be_approved_or_declined() {
    let world = World::linked();
    world.connected();

    let eve = "e5e5e5e5-0000-4000-8000-000000000005";
    let frank = "f6f6f6f6-0000-4000-8000-000000000006";
    world.steer(&format!("group-requesting {eve} Eve"));
    world.steer(&format!("group-requesting {frank} Frank"));
    world.until("two join requests listed", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. } if members.iter().filter(|m| m.status == "Requested to join").count() == 2 => Some(()),
            _ => None,
        }
    });

    // Approve Eve -> becomes a full member; deny Frank -> removed from requests.
    assert_eq!(
        world.ask(Request::MembersRemove { conversation: key(CLUB), person: format!("\u{1}signal-join-approve\u{1}{eve}") }),
        Response::Ok
    );
    world.heard(&format!("group-request {CLUB} {eve} approve"));
    assert_eq!(
        world.ask(Request::MembersRemove { conversation: key(CLUB), person: format!("\u{1}signal-join-deny\u{1}{frank}") }),
        Response::Ok
    );
    world.heard(&format!("group-request {CLUB} {frank} deny"));
    world.until("Eve approved and Frank denied", || {
        match world.ask(Request::Members { conversation: key(CLUB), offset: 0, limit: 50 }) {
            Response::Members { members, .. }
                if members.iter().any(|m| m.key.id == eve && m.status.is_empty())
                    && !members.iter().any(|m| m.key.id == frank) =>
            {
                Some(())
            }
            _ => None,
        }
    });

    // Accept / decline a pending group invite to me.
    assert_eq!(
        world.ask(Request::AnswerMessageRequest { conversation: key(CLUB), accept: true }),
        Response::Ok
    );
    world.heard(&format!("group-invite {CLUB} accept"));
    assert_eq!(
        world.ask(Request::AnswerMessageRequest { conversation: key(CLUB), accept: false }),
        Response::Ok
    );
    world.heard(&format!("group-invite {CLUB} decline"));
}

#[test]
fn searching_and_sending_gifs_from_the_composer() {
    let world = World::linked();
    world.connected();

    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|a| a.key == account()).unwrap();
    assert!(signal.can.gifs, "Signal advertises GIF picker capability");

    // Search GIFs via Catalog.
    let Response::Catalog { items, .. } = world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: "cat".into(),
        cursor: None,
    }) else {
        panic!("expected Catalog response");
    };
    world.heard("gif-search offset=0 query=cat");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "gif-cat");
    assert!(items[0].animated);
    assert_eq!(items[0].file.as_ref().map(|f| f.kind), Some(protocol::MediaKind::Gif));

    // Send the picked GIF: fetches MP4 and sends as looping GIF attachment.
    let Response::Sent { .. } = world.ask(Request::Send {
        conversation: key(CLUB),
        client_id: "gif-1".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            pick: Some(protocol::Pick { kind: protocol::CatalogKind::Gifs, id: "gif-cat".into() }),
            ..Outgoing::default()
        },
    }) else {
        panic!("expected Sent");
    };
    world.heard("gif-fetch https://media.giphy.com/media/cat/200w.mp4");
    world.heard(&format!("send {CLUB} files giphy.mp4"));
    let sent = world.until("sent GIF message", || {
        world
            .messages(CLUB)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("gif-1") && m.id != "gif-1")
    });
    assert_eq!(sent.attachments.len(), 1);
    assert_eq!(sent.attachments[0].file.kind, protocol::MediaKind::Gif);
    assert_eq!(sent.attachments[0].mime, "video/mp4");
}

#[test]
fn synced_call_events_and_call_log_events_record_and_clear_call_history() {
    let world = World::linked();
    world.connected();

    // Accepted incoming video call -> "Incoming video call", not missed (0 unread).
    world.steer(&format!("call-event grace 9001 video incoming accepted {}", T0 + 10_000));
    let call_msg = world.arrived(GRACE, "signal-call-9001");
    assert_eq!((&call_msg.kind, call_msg.text.as_str(), call_msg.outgoing), (&MessageKind::System, "Incoming video call", false));
    assert_eq!(world.conversation(GRACE).unwrap().unread, 0);

    // Outgoing accepted voice call -> "Outgoing voice call".
    world.steer(&format!("call-event grace 9002 audio outgoing accepted {}", T0 + 20_000));
    let out_msg = world.arrived(GRACE, "signal-call-9002");
    assert_eq!((&out_msg.kind, out_msg.text.as_str(), out_msg.outgoing), (&MessageKind::System, "Outgoing voice call", true));

    // Missed incoming voice call -> unread increments; then call-log read-chat clears unread.
    world.steer(&format!("call-event grace 9003 audio incoming missed {}", T0 + 30_000));
    let missed = world.arrived(GRACE, "signal-call-9003");
    assert_eq!(missed.text, "Missed voice call");
    world.until("missed call unread", || (world.conversation(GRACE)?.unread >= 1).then_some(()));
    world.steer("call-log read-chat grace");
    world.until("call-log read clears unread", || (world.conversation(GRACE)?.unread == 0).then_some(()));

    // Group call event in CLUB.
    world.steer(&format!("call-event club 9004 group incoming accepted {}", T0 + 40_000));
    let group_call = world.arrived(CLUB, "signal-call-9004");
    assert_eq!(group_call.text, "Group call");

    // Deleting a single call event removes it.
    world.steer(&format!("call-event grace 9002 audio outgoing deleted {}", T0 + 45_000));
    world.until("call 9002 deleted", || {
        (!world.messages(GRACE).iter().any(|m| m.id == "signal-call-9002")).then_some(())
    });

    // Clearing call history in Grace's conversation removes remaining call rows there, leaving CLUB untouched.
    world.steer("call-log clear-chat grace");
    world.until("grace call history cleared", || {
        (!world.messages(GRACE).iter().any(|m| m.id.starts_with("signal-call-"))).then_some(())
    });
    assert!(world.messages(CLUB).iter().any(|m| m.id == "signal-call-9004"));
}

#[test]
fn call_links_are_redacted_render_join_cards_and_can_be_created_synced_joined_and_deleted() {
    let world = World::linked();
    world.connected();

    // Create a call link with approval required.
    let Response::Link { url } = world.ask(Request::Handoff {
        conversation: key("\u{1}signal-call-link-create\u{1}1|Weekly standup"),
    }) else {
        panic!("expected Link with call link URL");
    };
    assert!(url.starts_with("https://signal.link/call/#key="));
    assert_eq!(protocol::redact(&url), "https://signal.link/call/#key=[redacted]");
    world.heard("call-link-create name=Weekly standup approve=true");

    // A message containing the call link renders a "Join call" card with its name and approval notice.
    world.steer(&format!("incoming grace {} Join the call {url}", T0 + 60_000));
    let msg = world.arrived(GRACE, &id(T0 + 60_000, GRACE));
    let card = msg.card.expect("call link message must render a Join call card");
    assert_eq!(card.title, "Weekly standup");
    assert_eq!(card.text, "Admin approval required to join.");
    assert_eq!(card.footer, "Join call");

    // Joining the approval-required call link reports waiting:<room_id>.
    let Response::Link { url: joined } = world.ask(Request::Handoff {
        conversation: key(&format!("\u{1}signal-call-link-join\u{1}{url}")),
    }) else {
        panic!("expected Link join response");
    };
    assert!(joined.starts_with("waiting:"), "expected waiting room state, got {joined}");

    // Synced call link from another device is accepted, and deleting our link removes it.
    world.steer("call-link-sync 99");
    let Response::Link { url: del } = world.ask(Request::Handoff {
        conversation: key(&format!("\u{1}signal-call-link-delete\u{1}{url}")),
    }) else {
        panic!("expected delete response");
    };
    assert_eq!(del, "deleted");
    world.heard("call-link-delete");
}

#[test]
fn storage_service_manifest_seeds_contacts_groups_pins_archive_and_mutes_on_first_connect() {
    let club_hex = CLUB.strip_prefix("g.").unwrap();
    let far_future: u64 = 4_102_444_800_000;
    let manifest = format!(
        r#"{{
            "contacts": [
                {{"aci":"{GRACE}","name":"Grace Ombra","nickname":"Gracie","note":"Bridge partner","phone":"+15550100002","username":"grace.42","whitelisted":true}},
                {{"aci":"{ALAN}","name":"Alan Ferro","archived":true,"muted_until":{far_future},"whitelisted":true}}
            ],
            "groups": [
                {{"id":"{club_hex}","title":"Bridge club","whitelisted":true}}
            ],
            "account": {{
                "pinned": ["{GRACE}"],
                "keep_muted_archived": true,
                "link_previews": true,
                "universal_expire_timer": 3600
            }}
        }}"#
    );
    let world = World::start_with_setup(Some(SESSION), |dir| {
        std::fs::write(dir.join("signal.storage.json"), &manifest).unwrap();
    });
    world.connected();

    // All 3 conversations (2 contacts + 1 group) are seeded on first connect with NO messages.
    let grace = world.until("Grace seeded and pinned", || {
        let row = world.conversation(GRACE)?;
        (row.title == "Gracie" && row.pin == Some(0)).then_some(row)
    });
    assert_eq!(grace.kind, ConversationKind::Direct);
    assert!(world.messages(GRACE).is_empty());

    let alan = world.until("Alan seeded archived and muted", || {
        let row = world.conversation(ALAN)?;
        (row.archived && row.muted).then_some(row)
    });
    assert_eq!(alan.title, "Alan Ferro");
    assert!(world.messages(ALAN).is_empty());

    let club = world.conversation(CLUB).expect("group seeded on first connect");
    assert_eq!(club.title, "Bridge club");
    assert!(world.messages(CLUB).is_empty());

    // Re-fetching an updated manifest (simulating SyncMessage.fetchLatest StorageManifest) updates pins and nicknames.
    let updated = format!(
        r#"{{"contacts":[{{"aci":"{GRACE}","name":"Grace Ombra","nickname":"G. Ombra","whitelisted":true}}],"groups":[{{"id":"{club_hex}","title":"Bridge club","whitelisted":true}}],"account":{{"pinned":["{CLUB}"]}}}}"#
    );
    world.steer(&format!("storage-manifest {updated}"));
    world.until("updated manifest moved pin to club and updated nickname", || {
        let g = world.conversation(GRACE)?;
        let c = world.conversation(CLUB)?;
        (g.title == "G. Ombra" && g.pin.is_none() && c.pin == Some(0)).then_some(())
    });
}

#[test]
fn safety_number_change_writes_system_rows_and_blocks_verified_send_until_approved() {
    let world = World::linked();
    world.connected();

    // Mark Grace verified via the Handoff verify command; returns formatted 60-digit safety number
    // (12 groups of 5 digits) and applies Mark::Verified to Grace's person row.
    let before_digits = match world.ask(Request::Handoff {
        conversation: key(&chatd::adapters::signal::safety_verify_command(GRACE)),
    }) {
        Response::Link { url } => url,
        other => panic!("expected formatted safety number, got {other:?}"),
    };
    let groups: Vec<&str> = before_digits.split(' ').collect();
    assert_eq!(groups.len(), 12, "expected 12 groups of 5 digits, got {before_digits:?}");
    assert!(groups.iter().all(|g| g.len() == 5 && g.bytes().all(|b| b.is_ascii_digit())));

    world.until("Grace gets Verified mark", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Signal),
            relation: None,
            query: Some("Grace".into()),
            offset: 0,
            limit: 10,
        }) else {
            return None;
        };
        let grace = people.iter().find(|p| p.key.id == GRACE)?;
        (grace.mark == Some(protocol::Mark::Verified)).then_some(())
    });

    // Simulate Grace's identity key changing: system row appears in DM and shared group,
    // Verified mark is removed, and sending to Grace is refused until approved.
    world.steer("identity-change grace");
    world.until("safety number change system line appears in DM and group", || {
        let dm = world.messages(GRACE);
        let grp = world.messages(CLUB);
        let dm_has = dm.iter().any(|m| {
            m.kind == MessageKind::System && m.text == "Your safety number with Grace Ombra has changed"
        });
        let grp_has = grp.iter().any(|m| {
            m.kind == MessageKind::System && m.text == "Your safety number with Grace Ombra has changed"
        });
        (dm_has && grp_has).then_some(())
    });
    world.until("Grace Verified mark cleared after key change", || {
        let Response::People { people, .. } = world.ask(Request::People {
            network: Some(Network::Signal),
            relation: None,
            query: Some("Grace".into()),
            offset: 0,
            limit: 10,
        }) else {
            return None;
        };
        let grace = people.iter().find(|p| p.key.id == GRACE)?;
        grace.mark.is_none().then_some(())
    });

    // Sending to Grace while changed & unapproved must be refused.
    world.send(GRACE, "blocked-id-1", "secret before approval", None);
    let failed = world.until("send refused while identity change unapproved", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("blocked-id-1") && matches!(m.state, SendState::Failed { .. }))
    });
    let SendState::Failed { reason } = failed.state else {
        unreachable!();
    };
    assert!(
        reason.to_lowercase().contains("safety number"),
        "expected safety number refusal, got: {reason}"
    );

    // Approve the new safety number: returns new 60-digit safety number (different from before),
    // clears the send block, and allows sending again.
    let after_digits = match world.ask(Request::Handoff {
        conversation: key(&chatd::adapters::signal::safety_approve_command(GRACE)),
    }) {
        Response::Link { url } => url,
        other => panic!("expected approved safety number, got {other:?}"),
    };
    assert_ne!(before_digits, after_digits, "rotated key must produce a different safety number");

    world.send(GRACE, "allowed-id-2", "after approval", None);
    world.until("send succeeds after approval", || {
        world
            .messages(GRACE)
            .iter()
            .any(|m| m.text == "after approval" && m.id != "allowed-id-2" && matches!(m.state, SendState::Sent))
            .then_some(())
    });
    assert!(world.calls().contains(&format!("verify {GRACE} verified")));
    assert!(world.calls().contains(&format!("verify {GRACE} default")));
}

#[test]
fn person_profile_returns_about_badges_masked_phone_username_shared_groups_nickname_note_and_safety_number() {
    const STRANGER: &str = "d4d4d4d4-0000-4000-8000-000000000004";
    let manifest = format!(
        r#"{{"contacts":[{{"aci":"{GRACE}","name":"Grace Ombra","nickname":"Bridge Grace","note":"Always opens 1NT","phone":"+15550100002","whitelisted":true}},{{"aci":"{STRANGER}","name":"","whitelisted":false}}],"groups":[{{"id":"{CLUB_HEX}","title":"Bridge club","whitelisted":true}}]}}"#,
        CLUB_HEX = CLUB.trim_start_matches("g.")
    );
    let world = World::start_with_setup(Some(SESSION), |support| {
        std::fs::write(support.join("signal.storage.json"), &manifest).unwrap();
    });
    world.connected();

    // Learn Grace's username via username lookup so it is stored and surfaced in her profile.
    world.steer(&format!("lookup grace.42 {GRACE}"));
    let (_, seq) = world.status();
    let _ = world.ask(Request::People {
        network: Some(Network::Signal),
        relation: None,
        query: Some("grace.42".into()),
        offset: 0,
        limit: 10,
    });
    world.changed(seq, "grace.42 lookup to finish", |change| match change {
        Change::Searched { query, .. } if query == "grace.42" => Some(()),
        _ => None,
    });

    // Fetch Grace's full profile.
    let Response::Profile { profile } = world.ask(Request::Profile {
        person: key(GRACE),
        space: None,
    }) else {
        panic!("expected Response::Profile for Grace");
    };
    assert!(profile.bio.contains("Likes bridge"), "expected decrypted about in bio: {:?}", profile.bio);
    assert!(profile.bio.contains("🃏"), "expected about emoji in bio: {:?}", profile.bio);
    assert_eq!(profile.nickname, "Bridge Grace");
    assert_eq!(profile.note, "Always opens 1NT");
    assert_eq!(profile.person.name, "Bridge Grace", "nickname must be preferred over profile name");
    assert_eq!(profile.person.handle, "grace.42");
    assert!(
        profile.contacts.iter().any(|c| c.kind == ContactKind::Phone && c.value == "+15550100002" && c.masked),
        "expected masked phone number in profile contacts: {:?}",
        profile.contacts
    );
    assert!(
        profile.mutual_spaces.iter().any(|s| s == "Bridge club"),
        "expected shared group Bridge club: {:?}",
        profile.mutual_spaces
    );
    assert!(
        profile.fields.iter().any(|(k, v)| k == "Badges" && v.contains("Sustainer")),
        "expected Badges field: {:?}",
        profile.fields
    );
    let safety = profile
        .fields
        .iter()
        .find(|(k, _)| k == "Safety number")
        .map(|(_, v)| v.as_str())
        .expect("expected Safety number field");
    assert_eq!(safety.split(' ').count(), 12);

    // A stranger whose profile key is unknown shows "Name not shared yet" instead of an empty bug state.
    let Response::Profile { profile: stranger } = world.ask(Request::Profile {
        person: key(STRANGER),
        space: None,
    }) else {
        panic!("expected Response::Profile for Stranger");
    };
    assert_eq!(stranger.bio, "Name not shared yet");
}

#[test]
fn link_previews_render_on_receive_ignore_mismatched_urls_and_honor_setting_and_suppress_on_send() {
    let world = World::linked();
    world.connected();

    // Receive: a preview whose URL appears in the message body populates Message::previews.
    world.steer("preview grace 1791892810000 https://example.org/bridge | Bridge Rules | Contract bridge guide | Read https://example.org/bridge tonight");
    let received = world.until("incoming preview message stored", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text.contains("https://example.org/bridge"))
    });
    assert_eq!(received.previews.len(), 1);
    assert_eq!(received.previews[0].url, "https://example.org/bridge");
    assert_eq!(received.previews[0].site, "example.org");
    assert_eq!(received.previews[0].title, "Bridge Rules");
    assert_eq!(received.previews[0].description, "Contract bridge guide");

    // Receive: a preview whose URL does NOT appear in the body must be ignored.
    world.steer("preview grace 1791892820000 https://evil.example.net/phish | Spoofed | Fake card | Check out https://example.org/safe");
    let spoofed = world.until("mismatched preview message stored", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text.contains("https://example.org/safe"))
    });
    assert!(
        spoofed.previews.is_empty(),
        "preview whose URL is not in body must be ignored, got {:?}",
        spoofed.previews
    );

    // Send with link previews enabled: fetches via Signal content proxy and attaches preview.
    world.send(GRACE, "lp-send-1", "Look at https://rust-lang.org/news", None);
    let sent_with_preview = world.until("outgoing preview echoed", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text == "Look at https://rust-lang.org/news" && m.id != "lp-send-1")
    });
    assert_eq!(sent_with_preview.previews.len(), 1);
    assert_eq!(sent_with_preview.previews[0].url, "https://rust-lang.org/news");
    assert_eq!(sent_with_preview.previews[0].title, "Preview for rust-lang.org");
    assert!(world.calls().contains(&"preview-fetch https://rust-lang.org/news".to_owned()));

    // Send with composer remove chip (`suppress_embeds: true`): skips preview fetch and sends no preview.
    let _ = world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "lp-send-2".into(),
        text: "No card for https://suppressed.example.org/page".into(),
        reply_to: None,
        with: Outgoing {
            suppress_embeds: true,
            ..Outgoing::default()
        },
    });
    let sent_suppressed = world.until("suppressed send stored", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text == "No card for https://suppressed.example.org/page" && m.id != "lp-send-2")
    });
    assert!(sent_suppressed.previews.is_empty());
    assert!(!world.calls().iter().any(|c| c.contains("suppressed.example.org") && c.starts_with("preview-fetch")));

    // Configuration sync disables "Generate link previews": subsequent sends fetch no preview.
    world.steer("link-previews off");
    world.until("capabilities drop suppress_embeds when link previews disabled", || {
        let (accounts, _) = world.status();
        let acct = accounts.iter().find(|a| a.key.network == Network::Signal)?;
        (!acct.can.suppress_embeds).then_some(())
    });
    world.send(GRACE, "lp-send-3", "Setting off https://disabled.example.org/page", None);
    let sent_disabled = world.until("disabled send stored", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.text == "Setting off https://disabled.example.org/page" && m.id != "lp-send-3")
    });
    assert!(sent_disabled.previews.is_empty());
    assert!(!world.calls().iter().any(|c| c.contains("disabled.example.org") && c.starts_with("preview-fetch")));
}

#[test]
fn pinned_messages_serve_pinned_list_expire_and_respect_group_permissions() {
    let world = World::linked();
    world.connected();

    // Grace sends two messages in DM.
    world.steer("incoming grace 1791892810000 Important bridge schedule");
    world.steer("incoming grace 1791892811000 Short-lived pin");
    world.until("both incoming messages stored", || {
        (world.messages(GRACE).len() >= 2).then_some(())
    });

    // Incoming pin from Grace (forever) emits system line and appears in Request::Pinned.
    world.steer("pin-msg grace grace grace 1791892810000");
    world.until("Grace pinned system line appears", || {
        world
            .messages(GRACE)
            .iter()
            .any(|m| m.kind == MessageKind::System && m.text == "Grace Ombra pinned a message")
            .then_some(())
    });
    let Response::Pinned { messages, .. } = world.ask(Request::Pinned { conversation: key(GRACE) }) else {
        panic!("expected Response::Pinned");
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "Important bridge schedule");
    assert!(messages[0].pinned);
    assert_eq!(messages[0].pinned_by.as_deref(), Some("Grace Ombra"));

    // Incoming expiring pin (1 second) appears and then drops once expired.
    world.steer("pin-msg grace grace grace 1791892811000 1");
    world.until("expiring pin recorded then expires", || {
        let Response::Pinned { messages, .. } = world.ask(Request::Pinned { conversation: key(GRACE) }) else {
            return None;
        };
        (messages.len() == 1
            && messages[0].text == "Important bridge schedule"
            && world.messages(GRACE).iter().filter(|m| m.text == "Grace Ombra pinned a message").count() >= 2)
            .then_some(())
    });

    // Outgoing unpin removes it and logs unpin call on the wire.
    let target_id = format!("1791892810000-{GRACE}");
    assert!(matches!(
        world.ask(Request::Pin {
            conversation: key(GRACE),
            message_id: target_id.clone(),
            pinned: false,
            notify: false,
            me_only: false,
        }),
        Response::Ok
    ));
    assert!(world.calls().contains(&format!("send {GRACE} unpin {target_id}")));
    let Response::Pinned { messages, .. } = world.ask(Request::Pinned { conversation: key(GRACE) }) else {
        panic!("expected Response::Pinned");
    };
    assert!(messages.is_empty());

    // Outgoing pin in DM works and logs pin call on the wire.
    assert!(matches!(
        world.ask(Request::Pin {
            conversation: key(GRACE),
            message_id: target_id.clone(),
            pinned: true,
            notify: true,
            me_only: false,
        }),
        Response::Ok
    ));
    assert!(world.calls().contains(&format!("send {GRACE} pin {target_id} dur=forever")));

    // Group permission enforcement: non-admin cannot pin when attributes=admin.
    world.steer("group grace 1791892812000 Group announcement");
    world.steer("group-role me member");
    world.steer("group-access attributes=admin");
    world.until("club updated with admin-only attributes", || {
        let c = world.conversation(CLUB)?;
        (!c.rights.pin && world.messages(CLUB).iter().any(|m| m.text == "Group announcement")).then_some(())
    });
    let group_msg_id = format!("1791892812000-{GRACE}");
    let denied = world.ask(Request::Pin {
        conversation: key(CLUB),
        message_id: group_msg_id,
        pinned: true,
        notify: true,
        me_only: false,
    });
    assert!(
        matches!(&denied, Response::Error { detail, .. } if detail.to_lowercase().contains("admin")),
        "expected admin permission refusal, got {denied:?}"
    );
}

#[test]
fn group_at_mentions_send_and_edit_as_ufffc_with_utf16_body_ranges_and_echo_names() {
    let world = World::linked();
    world.connected();

    // Send "hi <@GRACE>" in the group: wire gets U+FFFC + Mention(GRACE):3+1, local echo reads "hi @Grace Ombra".
    world.send(CLUB, "mention-1", &format!("hi <@{GRACE}>"), None);
    let echoed = world.until("group mention echoed with resolved name", || {
        world
            .messages(CLUB)
            .into_iter()
            .find(|m| m.text == "hi @Grace Ombra" && m.id != "mention-1")
    });
    assert!(
        echoed.spans.iter().any(|s| {
            matches!(&s.style, protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == GRACE)
                && echoed.text.get(s.start as usize..s.end as usize) == Some("@Grace Ombra")
        }),
        "expected Style::Mention span on echoed message: {:?}",
        echoed.spans
    );
    assert!(
        world
            .calls()
            .contains(&format!("send {CLUB} text hi \u{FFFC} ranges=Mention({GRACE}):3+1")),
        "expected wire body with U+FFFC and UTF-16 range, calls: {:?}",
        world.calls()
    );

    // Non-ASCII surrogate-pair prefix: 🎉 (2 UTF-16 code units) + " hi " (4) => offset 6.
    world.send(CLUB, "mention-2", &format!("🎉 hi <@{GRACE}>"), None);
    world.until("emoji mention echoed", || {
        world
            .messages(CLUB)
            .into_iter()
            .find(|m| m.text == "🎉 hi @Grace Ombra" && m.id != "mention-2")
    });
    assert!(
        world
            .calls()
            .contains(&format!("send {CLUB} text 🎉 hi \u{FFFC} ranges=Mention({GRACE}):6+1")),
        "expected UTF-16 offset 6 after surrogate pair, calls: {:?}",
        world.calls()
    );

    // Editing a message with <@ALAN> also sends U+FFFC + Mention(ALAN):8+1 and updates local echo.
    let edit_resp = world.ask(Request::Edit {
        conversation: key(CLUB),
        message_id: echoed.id.clone(),
        text: format!("updated <@{ALAN}>"),
        spans: Vec::new(),
    });
    assert!(matches!(edit_resp, Response::Ok), "expected edit Ok, got {edit_resp:?}");
    let edited = world.until("edited mention updated locally", || {
        world
            .messages(CLUB)
            .into_iter()
            .find(|m| m.id == echoed.id && m.text == "updated @Alan Ferro")
    });
    assert!(edited.edited);
    assert!(
        edited.spans.iter().any(|s| {
            matches!(&s.style, protocol::Style::Mention { kind: protocol::MentionKind::User, id } if id == ALAN)
        }),
        "expected Style::Mention span on edited message: {:?}",
        edited.spans
    );
    let sent_ts = echoed.id.split('-').next().unwrap();
    assert!(
        world
            .calls()
            .contains(&format!("send {CLUB} edit {sent_ts} updated \u{FFFC} ranges=Mention({ALAN}):8+1")),
        "expected edit wire body with U+FFFC and Mention(ALAN):8+1, calls: {:?}",
        world.calls()
    );
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Encrypts `plaintext` with Signal's attachment cipher (AES-256-CBC + HMAC-SHA256 + SHA-256 digest).
fn encrypt_signal_blob(plaintext: &[u8], key: &[u8; 64], iv: &[u8; 16]) -> (Vec<u8>, [u8; 32]) {
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Aes256;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    let aes = Aes256::new_from_slice(&key[..32]).unwrap();
    let pad = 16 - (plaintext.len() % 16);
    let mut out = Vec::with_capacity(16 + plaintext.len() + pad + 32);
    out.extend_from_slice(iv);
    let mut prev = *iv;
    let mut chunks = plaintext.chunks_exact(16);
    for chunk in &mut chunks {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        aes.encrypt_block((&mut block).into());
        out.extend_from_slice(&block);
        prev = block;
    }
    let rem = chunks.remainder();
    let mut last = [pad as u8; 16];
    last[..rem.len()].copy_from_slice(rem);
    for i in 0..16 {
        last[i] ^= prev[i];
    }
    aes.encrypt_block((&mut last).into());
    out.extend_from_slice(&last);

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key[32..64]).unwrap();
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());

    let digest: [u8; 32] = Sha256::digest(&out).into();
    (out, digest)
}

/// Spawns a tiny local HTTP stub serving `routes` (path -> body bytes, or 404 when absent).
fn spawn_cdn_stub(routes: Vec<(String, Vec<u8>)>) -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 2048];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if let Some((_, body)) = routes.iter().find(|(p, _)| p == path) {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body);
                    } else {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    let _ = stream.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}"), stop)
}

#[test]
fn received_photos_videos_gifs_albums_files_and_quote_thumbs_decrypt_from_cdn_and_populate_shared_media() {
    let photo_plain = b"\xff\xd8\xff\xe0FAKE-JPEG-PHOTO-PAYLOAD-740\xff\xd9".to_vec();
    let video_plain = b"\x00\x00\x00\x18ftypmp42FAKE-VIDEO-PAYLOAD-740".to_vec();
    let gif_plain = b"\x00\x00\x00\x18ftypmp42FAKE-GIF-PAYLOAD-740".to_vec();
    let pdf_plain = b"%PDF-1.7\n1 0 obj << /Title (Q4 Report) >> endobj\n%%EOF".to_vec();
    let album1_plain = b"\xff\xd8\xff\xe0ALBUM-ONE\xff\xd9".to_vec();
    let album2_plain = b"\xff\xd8\xff\xe0ALBUM-TWO\xff\xd9".to_vec();
    let quote_thumb_plain = b"\xff\xd8\xff\xe0QUOTED-THUMB\xff\xd9".to_vec();

    let key_photo = [0x11u8; 64];
    let key_video = [0x22u8; 64];
    let key_gif = [0x33u8; 64];
    let key_pdf = [0x44u8; 64];
    let key_a1 = [0x55u8; 64];
    let key_a2 = [0x66u8; 64];
    let key_qt = [0x77u8; 64];
    let iv = [0xabu8; 16];

    let (photo_enc, photo_dig) = encrypt_signal_blob(&photo_plain, &key_photo, &iv);
    let (video_enc, video_dig) = encrypt_signal_blob(&video_plain, &key_video, &iv);
    let (gif_enc, gif_dig) = encrypt_signal_blob(&gif_plain, &key_gif, &iv);
    let (pdf_enc, pdf_dig) = encrypt_signal_blob(&pdf_plain, &key_pdf, &iv);
    let (a1_enc, a1_dig) = encrypt_signal_blob(&album1_plain, &key_a1, &iv);
    let (a2_enc, a2_dig) = encrypt_signal_blob(&album2_plain, &key_a2, &iv);
    let (qt_enc, qt_dig) = encrypt_signal_blob(&quote_thumb_plain, &key_qt, &iv);

    let (base, stop_cdn) = spawn_cdn_stub(vec![
        ("/photo.enc".into(), photo_enc),
        ("/video.enc".into(), video_enc),
        ("/gif.enc".into(), gif_enc),
        ("/report.enc".into(), pdf_enc),
        ("/a1.enc".into(), a1_enc),
        ("/a2.enc".into(), a2_enc),
        ("/qt.enc".into(), qt_enc),
    ]);

    let world = World::linked();
    world.connected();

    // 1. Photo with BlurHash + caption
    world.steer(&format!(
        "media grace {} image/jpeg 1200 900 0 sunset.jpg {base}/photo.enc {} {} 00TI:j | Sunset over the bay",
        T0 + 1_000,
        hex_of(&key_photo),
        hex_of(&photo_dig),
    ));
    let photo_msg = world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    assert_eq!(photo_msg.kind, MessageKind::Normal);
    assert_eq!(photo_msg.text, "Sunset over the bay");
    assert_eq!(photo_msg.attachments.len(), 1);
    let photo_att = &photo_msg.attachments[0];
    assert_eq!(photo_att.file.kind, protocol::MediaKind::Image);
    assert_eq!((photo_att.file.width, photo_att.file.height), (1200, 900));
    assert!(photo_att.preview.as_ref().and_then(|p| p.path.as_ref()).is_some(), "BlurHash preview should land immediately");

    // Trigger download + decrypt via Request::Fetch
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![photo_att.file.id.clone()], size: None, retry: false });
    let photo_path = world.until("photo decrypted to disk", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == photo_msg.id)
            .and_then(|m| m.attachments.first().and_then(|a| a.file.path.clone()))
    });
    assert_eq!(std::fs::read(&photo_path).unwrap(), photo_plain);

    // 2. Video with BlurHash poster thumb
    world.steer(&format!(
        "media grace {} video/mp4 1920 1080 0 clip.mp4 {base}/video.enc {} {} 00TI:j",
        T0 + 2_000,
        hex_of(&key_video),
        hex_of(&video_dig),
    ));
    let video_msg = world.arrived(GRACE, &id(T0 + 2_000, GRACE));
    assert_eq!(video_msg.attachments[0].file.kind, protocol::MediaKind::Video);
    assert!(video_msg.attachments[0].thumb.is_some(), "video with BlurHash should expose a poster thumb");
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![video_msg.attachments[0].file.id.clone()], size: None, retry: false });
    let video_path = world.until("video decrypted to disk", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == video_msg.id)
            .and_then(|m| m.attachments.first().and_then(|a| a.file.path.clone()))
    });
    assert_eq!(std::fs::read(&video_path).unwrap(), video_plain);

    // 3. GIF (FLAG_GIF = 8)
    world.steer(&format!(
        "media grace {} video/mp4 480 270 8 wave.mp4 {base}/gif.enc {} {}",
        T0 + 3_000,
        hex_of(&key_gif),
        hex_of(&gif_dig),
    ));
    let gif_msg = world.arrived(GRACE, &id(T0 + 3_000, GRACE));
    assert_eq!(gif_msg.attachments[0].file.kind, protocol::MediaKind::Gif);
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![gif_msg.attachments[0].file.id.clone()], size: None, retry: false });
    let gif_path = world.until("gif decrypted to disk", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == gif_msg.id)
            .and_then(|m| m.attachments.first().and_then(|a| a.file.path.clone()))
    });
    assert_eq!(std::fs::read(&gif_path).unwrap(), gif_plain);

    // 4. Document / PDF file with original filename
    world.steer(&format!(
        "media grace {} application/pdf 0 0 0 report.pdf {base}/report.enc {} {} - | Q4 summary",
        T0 + 4_000,
        hex_of(&key_pdf),
        hex_of(&pdf_dig),
    ));
    let pdf_msg = world.arrived(GRACE, &id(T0 + 4_000, GRACE));
    assert_eq!(pdf_msg.attachments[0].file.kind, protocol::MediaKind::File);
    assert_eq!(pdf_msg.attachments[0].name, "report.pdf");
    assert_eq!(pdf_msg.text, "Q4 summary");
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![pdf_msg.attachments[0].file.id.clone()], size: None, retry: false });
    let pdf_path = world.until("pdf decrypted to disk", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == pdf_msg.id)
            .and_then(|m| m.attachments.first().and_then(|a| a.file.path.clone()))
    });
    assert_eq!(std::fs::read(&pdf_path).unwrap(), pdf_plain);

    // 5. Multi-attachment album
    world.steer(&format!(
        "album grace {} image/jpeg|one.jpg|{base}/a1.enc|{}|{} ; image/jpeg|two.jpg|{base}/a2.enc|{}|{}",
        T0 + 5_000,
        hex_of(&key_a1),
        hex_of(&a1_dig),
        hex_of(&key_a2),
        hex_of(&a2_dig),
    ));
    let album_msg = world.arrived(GRACE, &id(T0 + 5_000, GRACE));
    assert_eq!(album_msg.attachments.len(), 2);
    for att in &album_msg.attachments {
        let _ = world.ask(Request::Fetch { account: account(), ids: vec![att.file.id.clone()], size: None, retry: false });
    }
    let album_paths = world.until("both album photos decrypted", || {
        let m = world.messages(GRACE).into_iter().find(|m| m.id == album_msg.id)?;
        let p0 = m.attachments.first()?.file.path.clone()?;
        let p1 = m.attachments.get(1)?.file.path.clone()?;
        Some((p0, p1))
    });
    assert_eq!(std::fs::read(&album_paths.0).unwrap(), album1_plain);
    assert_eq!(std::fs::read(&album_paths.1).unwrap(), album2_plain);

    // 6. Quoted photo reply carries ReplyTo::thumb and decrypts it
    world.steer(&format!(
        "quote-photo grace {} {} {base}/qt.enc {} {} | Love this shot!",
        T0 + 6_000,
        T0 + 1_000,
        hex_of(&key_qt),
        hex_of(&qt_dig),
    ));
    let quote_msg = world.arrived(GRACE, &id(T0 + 6_000, GRACE));
    let reply = quote_msg.reply_to.as_ref().expect("quoted photo reply_to");
    assert_eq!(reply.snippet, "Photo");
    let thumb = reply.thumb.as_ref().expect("quoted photo thumbnail");
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![thumb.id.clone()], size: None, retry: false });
    let thumb_path = world.until("quoted thumbnail decrypted", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == quote_msg.id)
            .and_then(|m| m.reply_to?.thumb?.path)
    });
    assert_eq!(std::fs::read(&thumb_path).unwrap(), quote_thumb_plain);

    // 7. Expired CDN attachment reports "No longer available"
    world.steer(&format!(
        "media grace {} image/jpeg 640 480 0 gone.jpg {base}/missing-404.enc {} {}",
        T0 + 7_000,
        hex_of(&key_photo),
        hex_of(&photo_dig),
    ));
    let gone_msg = world.arrived(GRACE, &id(T0 + 7_000, GRACE));
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![gone_msg.attachments[0].file.id.clone()], size: None, retry: false });
    let failed_reason = world.until("expired attachment marked failed", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.id == gone_msg.id)
            .and_then(|m| m.attachments.first()?.file.failed.clone())
    });
    assert_eq!(failed_reason, "No longer available");

    // 8. Request::Shared returns Media (photos/videos/gifs) and Files (PDF)
    let Response::Shared { items: media_items, .. } = world.ask(Request::Shared {
        conversation: key(GRACE),
        kind: protocol::SharedKind::Media,
        before: None,
        limit: 50,
        search: false,
    }) else {
        panic!("expected Response::Shared for SharedKind::Media");
    };
    assert!(media_items.iter().any(|m| m.message_id == photo_msg.id), "photo should appear in SharedKind::Media");
    assert!(media_items.iter().any(|m| m.message_id == video_msg.id), "video should appear in SharedKind::Media");
    assert!(media_items.iter().any(|m| m.message_id == album_msg.id), "album should appear in SharedKind::Media");

    let Response::Shared { items: file_items, .. } = world.ask(Request::Shared {
        conversation: key(GRACE),
        kind: protocol::SharedKind::Files,
        before: None,
        limit: 50,
        search: false,
    }) else {
        panic!("expected Response::Shared for SharedKind::Files");
    };
    assert!(file_items.iter().any(|m| m.message_id == pdf_msg.id), "PDF should appear in SharedKind::Files");

    stop_cdn.store(true, Ordering::Relaxed);
}

#[test]
fn voice_messages_download_populate_waveform_send_viewed_receipt_and_can_be_sent() {
    let world = World::linked();
    world.connected();

    // Signal advertises voice_notes capability.
    let (accounts, _) = world.status();
    let signal = accounts.iter().find(|s| s.key == account()).expect("Signal account");
    assert!(signal.can.voice_notes, "Signal should advertise Capabilities::voice_notes");

    // 1. Incoming voice note arrives as MediaKind::Audio with non-empty initial waveform.
    world.steer(&format!("voice grace {}", T0 + 10_000));
    let voice_msg = world.arrived(GRACE, &id(T0 + 10_000, GRACE));
    assert_eq!(voice_msg.kind, MessageKind::Normal);
    assert_eq!(voice_msg.attachments.len(), 1);
    assert_eq!(voice_msg.attachments[0].file.kind, protocol::MediaKind::Audio);
    assert!(!voice_msg.attachments[0].waveform.is_empty(), "voice note should render as Shape::Voice");

    // 2. Fetching the voice note decrypts/lands it and populates exact duration (2s WAV) and waveform.
    let file_id = voice_msg.attachments[0].file.id.clone();
    let _ = world.ask(Request::Fetch { account: account(), ids: vec![file_id.clone()], size: None, retry: false });
    let landed = world.until("voice message downloaded with real duration", || {
        let msg = world.messages(GRACE).into_iter().find(|m| m.id == voice_msg.id)?;
        let att = msg.attachments.first()?;
        (att.file.path.is_some() && att.duration == 2 && att.waveform.len() == 48).then(|| att.clone())
    });
    let voice_path = landed.file.path.clone().unwrap();

    // 3. Starting playback sends a Signal `viewed` receipt with receipts=true.
    let play_resp = world.ask(Request::Play(protocol::PlayRequest::Start {
        account: account(),
        file: file_id.clone(),
        path: voice_path.clone(),
        conversation: key(GRACE),
        message: voice_msg.id.clone(),
        title: "Voice message".into(),
        at: Some(0),
        rate: 100,
        output: None,
    }));
    assert_eq!(play_resp, Response::Ok);
    world.heard(&format!("viewed receipts=true {}", voice_msg.id));
    let _ = world.ask(Request::Play(protocol::PlayRequest::Stop));

    // With receipts off on Signal, playing sends viewed sync with receipts=false.
    world.steer("receipts off");
    world.steer(&format!("voice alan {}", T0 + 11_000));
    let alan_voice = world.arrived(ALAN, &id(T0 + 11_000, ALAN));
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec![alan_voice.attachments[0].file.id.clone()],
        size: None,
        retry: false,
    });
    let alan_path = world.until("alan voice landed", || {
        world
            .messages(ALAN)
            .into_iter()
            .find(|m| m.id == alan_voice.id)
            .and_then(|m| m.attachments.first()?.file.path.clone())
    });
    assert_eq!(
        world.ask(Request::Play(protocol::PlayRequest::Start {
            account: account(),
            file: alan_voice.attachments[0].file.id.clone(),
            path: alan_path,
            conversation: key(ALAN),
            message: alan_voice.id.clone(),
            title: "Voice message".into(),
            at: Some(0),
            rate: 100,
            output: None,
        })),
        Response::Ok
    );
    world.heard(&format!("viewed receipts=false {}", alan_voice.id));
    let _ = world.ask(Request::Play(protocol::PlayRequest::Stop));

    // 4. Sending a recorded voice note goes to Signal with voice=true and keeps duration + waveform.
    let outbox = world.dir.join("outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    let recorded_path = outbox.join("note.ogg");
    std::fs::write(&recorded_path, b"OggSfake-opus-voice-note-payload").unwrap();
    let sent_resp = world.ask(Request::Send {
        conversation: key(GRACE),
        client_id: "voice-send-1".into(),
        text: String::new(),
        reply_to: None,
        with: Outgoing {
            files: vec![OutgoingFile {
                path: recorded_path.to_string_lossy().into_owned(),
                name: "voice-message.ogg".into(),
                spoiler: false,
                view_once: false,
                as_file: false,
                original: false,
                media: None,
            }],
            voice: Some(protocol::VoiceNote { duration: 4_200, waveform: vec![40, 120, 200, 90] }),
            ..Outgoing::default()
        },
    });
    assert!(matches!(sent_resp, Response::Sent { .. }), "expected Sent, got {sent_resp:?}");
    world.heard(&format!("send {GRACE} files voice-message.ogg voice=true"));
    let echoed = world.until("sent voice note stored with waveform and duration", || {
        world
            .messages(GRACE)
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("voice-send-1") && m.id != "voice-send-1")
    });
    assert_eq!(echoed.attachments.len(), 1);
    assert_eq!(echoed.attachments[0].file.kind, protocol::MediaKind::Audio);
    assert_eq!(echoed.attachments[0].duration, 5);
    assert_eq!(echoed.attachments[0].waveform, vec![40, 120, 200, 90]);
}
