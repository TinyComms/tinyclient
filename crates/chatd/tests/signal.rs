//! The Signal adapter through chatd's real socket, with a scripted wire where Signal would be
//! (`adapters/signal/fake.rs`): the account, its contacts and its group are invented there, and
//! each test steers it by appending a line to a file in its scratch directory.
//!
//! What this proves is everything above the wire: what the adapter asks Signal for and how it maps
//! what comes back into the contract, and the link steps. It proves nothing about presage and
//! libsignal underneath, which no test can reach without a real account and a phone to link from.

use protocol::{
    read_frame, write_frame, AccountKey, Change, ChatStream, Conversation, ConversationKind, ErrorCode, Key, Link, Message, MessageKind,
    Network, Page, Relation, Request, Response, SendState, SignInInput, SignInProblem, SignInStep,
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
/// 2026-10-13 12:00 UTC in milliseconds, and what the fake sends with from there on.
const T0: u64 = 1_791_892_800_000;
const FIRST_SENT: u64 = T0 + 1_000_000;

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
    let sent = world.arrived(GRACE, &id(FIRST_SENT, ME));
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
fn reactions_edits_deletes_and_receipts_land_on_the_right_message() {
    let world = World::linked();
    world.connected();
    world.steer(&format!("incoming grace {} Lunch?", T0 + 1_000));
    let theirs = world.arrived(GRACE, &id(T0 + 1_000, GRACE));
    world.send(GRACE, "3001", "Sure", None);
    let ours = world.arrived(GRACE, &id(FIRST_SENT, ME));

    // Theirs: edited, reacted to (one reaction a person: the second replaces the first), deleted.
    world.steer(&format!("edit grace {} Lunch at noon?", T0 + 1_000));
    world.until("the edit", || world.messages(GRACE).into_iter().find(|m| m.id == theirs.id && m.edited && m.text == "Lunch at noon?"));
    world.steer(&format!("react grace me {FIRST_SENT} 👍"));
    world.until("their reaction", || world.messages(GRACE).into_iter().find(|m| m.id == ours.id && m.reactions.iter().any(|r| r.emoji == "👍")));
    world.steer(&format!("react grace me {FIRST_SENT} ❤️"));
    let replaced = world.until("the second reaction", || {
        world.messages(GRACE).into_iter().find(|m| m.id == ours.id && m.reactions.iter().any(|r| r.emoji == "❤️"))
    });
    assert!(!replaced.reactions.iter().any(|r| r.emoji == "👍"), "{:?}", replaced.reactions);
    world.steer(&format!("delete grace {}", T0 + 1_000));
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
    world.heard(&format!("send {GRACE} edit {FIRST_SENT} Sure!"));
    assert!(world.messages(GRACE).iter().any(|m| m.id == ours.id && m.edited && m.text == "Sure!"));
    let refused = world.ask(Request::Edit { conversation: key(GRACE), message_id: again.id.clone(), text: "Not mine".into() });
    assert!(matches!(&refused, Response::Error { detail, .. } if detail.contains("own messages")), "{refused:?}");
    // Grace read ours.
    world.steer(&format!("seen grace {FIRST_SENT}"));
    world.until("seen", || world.conversation(GRACE)?.seen.then_some(()));
    assert_eq!(world.ask(Request::Delete { conversation: key(GRACE), message_id: ours.id.clone() }), Response::Ok);
    world.heard(&format!("send {GRACE} delete {FIRST_SENT}"));
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
    world.ask(Request::SignOut { account: account() });
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
