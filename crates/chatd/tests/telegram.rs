//! The Telegram adapter through chatd's real socket, with a scripted wire where Telegram would
//! be (`adapters/telegram/fake.rs`): the account, its people and its messages are invented there,
//! and each test steers it by writing a line to a FIFO in its scratch directory.
//!
//! What this proves is everything above the wire: which requests the adapter makes and how it
//! maps what comes back into the contract, and the sign-in steps. It proves nothing about the
//! MTProto library underneath, which no test can reach without a registered app id and a real
//! account.

use protocol::{
    read_frame, write_frame, AccountKey, Change, CodeVia, Conversation, ConversationKind, ErrorCode, Group, Key, Link,
    MediaKind, MentionKind, Message, MessageKind, Network, Page, Presence, Relation, Request, Response, SendState,
    SignInInput, SignInProblem, SignInStep, Style, Watching,
};
use std::io::Write;
use protocol::ChatStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const SESSION: &str = r#"{"user":7001,"name":"Mira Kessler","home":2,"dcs":[]}"#;
/// What the fake serves for any file: longer than one piece, so it takes two requests.
const FILE_BYTES: u64 = 256 * 1024 + 9;

fn key(id: &str) -> Key {
    Key { network: Network::Telegram, account: "7001".into(), id: id.into() }
}

fn account() -> AccountKey {
    AccountKey { network: Network::Telegram, account: "7001".into() }
}

struct World {
    child: Child,
    dir: PathBuf,
}

impl World {
    /// chatd with the account already signed in.
    fn signed_in() -> World {
        World::start(Some(SESSION), "30")
    }

    /// `session` is the stored sign-in, if any; `qr_seconds` how long the fake's QR codes last.
    fn start(session: Option<&str>, qr_seconds: &str) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = PathBuf::from(format!("/tmp/tinychat-tg-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_chatd"));
        command
            .env("TINYCHAT_DIR", &dir)
            .env("TINYCHAT_TELEGRAM_FAKE", qr_seconds)
            .env_remove("TINYCHAT_SESSION_DISCORD")
            .env_remove("TINYCHAT_SESSION_SLACK")
            .env_remove("TINYCHAT_SESSION_TELEGRAM")
            .stderr(Stdio::from(log));
        if let Some(session) = session {
            command.env("TINYCHAT_SESSION_TELEGRAM", session);
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

    fn until<T>(&self, what: &str, look: impl FnMut() -> Option<T>) -> T {
        self.within(Duration::from_secs(20), what, look)
    }

    fn within<T>(&self, limit: Duration, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(found) = look() {
                return found;
            }
            if Instant::now() > deadline {
                panic!("waited {limit:?} for {what}; Telegram was asked: {:#?}; chatd said: {}", self.calls(), self.stderr());
            }
            sleep(Duration::from_millis(30));
        }
    }

    /// Every request the fake was sent, one line each.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("telegram.calls")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn called(&self, start: &str) -> usize {
        self.calls().iter().filter(|call| call.starts_with(start)).count()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.dir.join("stderr.log")).unwrap_or_default()
    }

    /// Tells the fake what Telegram says next.
    fn steer(&self, line: &str) {
        self.write_to("telegram.ctl", line);
    }

    fn steer_sign_in(&self, line: &str) {
        self.write_to("telegram.signin.ctl", line);
    }

    fn write_to(&self, fifo: &str, line: &str) {
        let path = self.dir.join(fifo);
        self.until("the fake to listen", || path.exists().then_some(()));
        // Blocks until the fake is reading, which it is whenever it is alive.
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn status(&self) -> (Vec<protocol::AccountStatus>, u64) {
        match self.ask(Request::Status) {
            Response::Status { accounts, seq, .. } => (accounts, seq),
            other => panic!("{other:?}"),
        }
    }

    fn connected(&self) {
        self.until("Telegram to connect", || self.status().0.iter().any(|account| account.link == Link::Connected).then_some(()));
        self.until("the chats", || self.conversation("u7003").map(drop));
    }

    fn conversation(&self, id: &str) -> Option<Conversation> {
        match self.ask(Request::Conversation { conversation: key(id) }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    fn tree(&self) -> (Vec<Group>, Vec<Conversation>) {
        match self.ask(Request::Tree { space: key("chats"), offset: 0, limit: 500, peek: true }) {
            Response::Tree { groups, conversations, .. } => (groups, conversations),
            other => panic!("{other:?}"),
        }
    }

    /// The newest page, asked as the shell asks when the conversation is opened.
    fn messages(&self, id: &str) -> Vec<Message> {
        match self.ask(Request::Messages { conversation: key(id), page: Page::Newest, limit: 100 }) {
            Response::Messages { messages, .. } => messages,
            _ => Vec::new(),
        }
    }

    fn message(&self, conversation: &str, id: &str) -> Option<Message> {
        self.messages(conversation).into_iter().find(|message| message.id == id)
    }

    fn arrived(&self, conversation: &str, id: &str) -> Message {
        self.until(&format!("message {id} in {conversation}"), || self.message(conversation, id))
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
            None => Request::SignInBegin { network: Network::Telegram },
            Some(input) => Request::SignInAnswer { network: Network::Telegram, input },
        };
        match self.ask(request) {
            Response::SignIn { network: Network::Telegram, step } => step,
            other => panic!("{other:?}"),
        }
    }

    /// The QR code on show, once Telegram has issued one.
    fn code_shown(&self) -> (String, i64) {
        self.until("a QR code", || match self.sign_in(None) {
            SignInStep::Qr { link, expires, problem: None } if !link.is_empty() => Some((link, expires)),
            _ => None,
        })
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

fn ranges(message: &Message) -> Vec<(&str, &Style)> {
    message.spans.iter().map(|span| (&message.text[span.start as usize..span.end as usize], &span.style)).collect()
}

#[test]
fn the_chats_come_as_one_space_with_folders_as_groups_and_everything_in_the_inbox() {
    let world = World::signed_in();
    world.connected();
    let (accounts, _) = world.status();
    assert_eq!((accounts[0].name.as_str(), accounts[0].me.as_str(), accounts[0].key.account.as_str()), ("Mira Kessler", "u7001", "7001"));
    let Response::Spaces { spaces } = world.ask(Request::Spaces { network: Some(Network::Telegram) }) else {
        panic!("no spaces");
    };
    assert_eq!(spaces.iter().map(|space| space.name.as_str()).collect::<Vec<_>>(), vec!["Telegram"]);

    // Pinned first, then the folder, then the rest, then the archive: each chat under one group.
    let (groups, rows) = world.tree();
    assert_eq!(groups.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(), vec!["Pinned", "Work", "Chats", "Archived"]);
    let placed: Vec<(&str, &str, ConversationKind)> = rows.iter().map(|row| (row.key.id.as_str(), row.group_title.as_str(), row.kind)).collect();
    assert_eq!(
        placed,
        vec![
            ("u7002", "Pinned", ConversationKind::Direct),
            ("g8001", "Work", ConversationKind::Group),
            ("c9002", "Work", ConversationKind::Forum),
            ("c9001", "Chats", ConversationKind::Channel),
            ("u7001", "Chats", ConversationKind::Direct),
            ("u7003", "Archived", ConversationKind::Direct),
        ]
    );
    let row = |id: &str| rows.iter().find(|row| row.key.id == id).unwrap();
    assert_eq!((row("u7002").title.as_str(), row("u7002").unread, row("u7002").last_read.as_deref()), ("Odalys Brandt", 1, Some("11")));
    assert_eq!(row("u7002").preview, "Good. Trailhead at seven.");
    assert_eq!(row("u7002").peer.as_deref(), Some("u7002"));
    assert_eq!(row("u7002").call, protocol::CallSupport::Handoff);
    assert_eq!(row("u7001").title, "Saved Messages");
    // The channel is muted on Telegram and only its owners post in it.
    assert!(row("c9001").muted && row("c9001").read_only && !row("g8001").muted);

    // Every Telegram chat is in the Inbox, channels included.
    let Response::Conversations { total, .. } = world.ask(Request::Inbox { network: Some(Network::Telegram), filter: Default::default(), offset: 0, limit: 50 }) else {
        panic!("no inbox");
    };
    assert_eq!(total, 6);

    // The contacts are the list the user owns, with what Telegram says of each.
    let people = world.until("the contacts", || {
        let asked = Request::People { network: Some(Network::Telegram), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 };
        match world.ask(asked) {
            Response::People { people, .. } if !people.is_empty() => Some(people),
            _ => None,
        }
    });
    assert_eq!((people[0].name.as_str(), people[0].handle.as_str(), people[0].presence), ("Odalys Brandt", "@odalys", Presence::Online));
    assert_eq!(people[0].link, "https://t.me/odalys");

    // The call button's link opens the chat in Telegram's own app.
    assert_eq!(world.ask(Request::Handoff { conversation: key("u7003") }), Response::Link { url: "tg://user?id=7003".into() });
}

#[test]
fn messages_arrive_change_and_go_and_what_they_hold_is_mapped() {
    let world = World::signed_in();
    world.connected();
    // The conversation is on screen, so what a new message shows is fetched as it arrives.
    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));

    world.steer("incoming");
    let rich = world.arrived("u7002", "13");
    assert_eq!(rich.text, "Café at 🦀 dock, ask Tobin: stove.fuel() secret");
    assert_eq!(
        ranges(&rich),
        vec![
            ("Café", &Style::Bold),
            ("🦀", &Style::Link { url: "https://example.org/dock".into() }),
            ("Tobin", &Style::Mention { kind: MentionKind::User, id: "u7003".into() }),
            ("stove.fuel()", &Style::Code),
            ("secret", &Style::Spoiler),
        ]
    );
    assert_eq!((rich.sender.id.as_str(), rich.sender.name.as_str(), rich.outgoing), ("u7002", "Odalys Brandt", false));
    assert_eq!(world.conversation("u7002").unwrap().unread, 2);

    world.steer("react");
    let reacted = world.until("the reactions", || world.message("u7002", "13").filter(|message| !message.reactions.is_empty()));
    let seen: Vec<(&str, u32, bool)> = reacted.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.count, reaction.mine)).collect();
    assert_eq!(seen, vec![("👍", 2, false), ("🔥", 1, false)]);

    world.steer("edit");
    let edited = world.until("the edit", || world.message("u7002", "13").filter(|message| message.edited));
    assert_eq!((edited.text.as_str(), edited.spans.len()), ("Café at the dock, seven sharp", 0));
    // An edit is not a new unread message.
    assert_eq!(world.conversation("u7002").unwrap().unread, 2);

    // A photo: its sizes are known before any byte, and the row's thumbnail is fetched from the
    // datacenter that holds it, in pieces, into the media cache.
    world.steer("photo");
    let photo = world.arrived("u7002", "14");
    let attachment = &photo.attachments[0];
    assert_eq!((attachment.file.id.as_str(), attachment.file.kind, attachment.file.width, attachment.file.height), ("p6014x", MediaKind::Image, 800, 600));
    let thumb = attachment.thumb.as_ref().unwrap();
    assert_eq!((thumb.id.as_str(), thumb.width, thumb.height), ("p6014m", 320, 240));
    let landed = world.until("the thumbnail on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["p6014m".into()], size: None, retry: false }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), FILE_BYTES);
    assert!(landed.contains("/media/telegram/7001/"), "{landed}");
    let calls = world.calls();
    assert!(calls.contains(&"ExportAuthorization 4".to_owned()) && calls.contains(&"ImportAuthorization".to_owned()), "{calls:#?}");
    assert_eq!(world.called("GetFile photo 6014 m"), 2, "{calls:#?}");
    // The stored message now names the file where it is.
    assert_eq!(world.message("u7002", "14").unwrap().attachments[0].thumb.as_ref().unwrap().path.as_deref(), Some(landed.as_str()));
    // The original is not fetched for a row, and asking for it does not mark it failed.
    let Response::Media { files } = world.ask(Request::Fetch { account: account(), ids: vec!["p6014x".into()], size: None, retry: false }) else {
        panic!("no answer");
    };
    assert!(files.is_empty());

    world.steer("voice");
    let voice = world.arrived("u7002", "15");
    let note = &voice.attachments[0];
    assert_eq!((note.name.as_str(), note.file.kind, note.duration, note.mime.as_str()), ("Voice message", MediaKind::Audio, 7, "audio/ogg"));
    assert_eq!(note.waveform, vec![255, 0, 255, 0, 255, 0, 255, 0]);

    // Forwarded from the channel, in answer to an earlier message.
    world.steer("forward");
    let forwarded = world.arrived("u7002", "16");
    let from = forwarded.forwarded.as_ref().unwrap();
    assert_eq!((from.from_title.as_str(), from.link.as_str()), ("Field Notes", "https://t.me/fieldnotes/5"));
    let reply = forwarded.reply_to.as_ref().unwrap();
    assert_eq!((reply.id.as_str(), reply.sender.as_str(), reply.snippet.as_str()), ("12", "Odalys Brandt", "Good. Trailhead at seven."));
    assert_eq!(reply.person, Some(key("u7002")));

    world.steer("delete");
    world.until("the message to go", || world.message("u7002", "13").is_none().then_some(()));

    // What tinychat cannot draw is named and linked, never dropped; a service message is a line.
    world.steer("poll");
    let poll = world.arrived("g8001", "21");
    assert_eq!(poll.text, "Poll: Saturday or Sunday?");
    assert_eq!(poll.kind, MessageKind::Unsupported { link: "tg://openmessage?chat_id=8001&message_id=21".into() });
    world.steer("service");
    let renamed = world.arrived("g8001", "22");
    assert_eq!((renamed.kind, renamed.text.as_str()), (MessageKind::System, "Tobin Reyes renamed this to \"Ridge crew\""));
}

#[test]
fn a_forum_topic_is_a_thread_under_the_forum() {
    let world = World::signed_in();
    world.connected();
    // A message in a topic nobody has listed yet: the topic is found and becomes a thread.
    world.steer("topic");
    let found = world.arrived("c9002_31", "42");
    assert_eq!((found.text.as_str(), found.sender.name.as_str()), ("Found it, bottom drawer.", "Tobin Reyes"));
    let thread = world.conversation("c9002_31").unwrap();
    assert_eq!((thread.kind, thread.title.as_str(), thread.parent.as_deref(), thread.root.as_deref()), (ConversationKind::Thread, "Tools", Some("c9002"), Some("31")));
    assert!(world.conversation("c9002_1").is_some_and(|general| general.title == "General"));
    // The forum's own row holds no messages: opening it lists its topics again.
    assert!(world.messages("c9002").is_empty());
    world.until("the topics to be asked for", || (world.called("GetForumTopics c9002") >= 2).then_some(()));

    // Opening the thread reads the topic's own history; a post in it names the topic.
    world.until("the topic's history", || world.message("c9002_31", "41"));
    assert_eq!(world.called("GetReplies c9002_31"), 1);
    world.send("c9002_31", "9001", "On my way", None);
    world.until("the post to be sent", || world.calls().contains(&"SendMessage c9002_31 On my way".to_owned()).then_some(()));
    world.until("it to be in the thread", || world.messages("c9002_31").into_iter().find(|message| message.text == "On my way" && message.state == SendState::Sent));

    // Read in a topic is told for the topic.
    world.ask(Request::MarkRead { conversation: key("c9002_31"), message_id: "42".into() });
    world.until("the topic to be read", || world.calls().contains(&"ReadDiscussion c9002_31 42".to_owned()).then_some(()));
}

#[test]
fn typing_reading_and_presence_go_both_ways_and_are_never_withheld() {
    let world = World::signed_in();
    world.connected();
    let watch = || match world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), people: vec![key("u7002"), key("u7003")], ..Watching::default() })) {
        Response::Watching { people, typists } => (people, typists),
        other => panic!("{other:?}"),
    };
    world.steer("typing");
    let typists = world.until("Odalys typing", || Some(watch().1).filter(|typists| !typists.is_empty()));
    assert_eq!((typists[0].id.as_str(), typists[0].name.as_str()), ("u7002", "Odalys Brandt"));
    world.steer("typing-stop");
    world.until("the typing to stop", || watch().1.is_empty().then_some(()));

    // Offline is said only where Telegram says it; a hidden last-seen is not offline.
    world.steer("status");
    world.until("Odalys offline", || watch().0.iter().any(|person| person.key.id == "u7002" && person.presence == Presence::Offline).then_some(()));
    assert!(watch().0.iter().all(|person| person.key.id != "u7003" || person.presence == Presence::Unknown));

    // Another device read everything.
    world.steer("read");
    let read = world.until("the chat to be read", || world.conversation("u7002").filter(|row| row.unread == 0));
    assert_eq!(read.last_read.as_deref(), Some("14"));

    // Ours: typing is told while the user types, paced; reading is told when the shell says a
    // message was seen; and acting says we are here. No setting holds any of it back.
    for _ in 0..3 {
        world.ask(Request::Typing { conversation: key("u7003") });
    }
    world.until("typing to be told", || (world.called("SetTyping u7003") == 1).then_some(()));
    world.ask(Request::MarkRead { conversation: key("g8001"), message_id: "20".into() });
    world.ask(Request::MarkRead { conversation: key("c9001"), message_id: "5".into() });
    world.until("reading to be told", || world.calls().contains(&"channels.ReadHistory c9001 5".to_owned()).then_some(()));
    sleep(Duration::from_millis(200));
    assert_eq!(world.called("SetTyping u7003"), 1);
    // g8001 was already read up to 20, so there was nothing to tell for it.
    assert_eq!((world.called("ReadHistory g8001"), world.called("UpdateStatus")), (0, 1));
}

#[test]
fn sending_editing_deleting_and_reacting_wait_for_telegram() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));

    let pending = world.send("u7002", "5001", "See you there", Some("12"));
    assert_eq!(pending.state, SendState::Pending);
    let sent = world.until("the message to be sent", || world.messages("u7002").into_iter().find(|message| message.text == "See you there" && message.state == SendState::Sent));
    assert_eq!((sent.id.as_str(), sent.client_id.as_deref(), sent.outgoing), ("101", Some("5001"), true));
    assert_eq!(sent.reply_to.as_ref().map(|reply| reply.id.as_str()), Some("12"));
    // The echo on the update stream and the answer to the request are one message.
    sleep(Duration::from_millis(300));
    assert_eq!(world.messages("u7002").iter().filter(|message| message.text == "See you there").count(), 1);
    assert!(world.calls().contains(&"SendMessage u7002 See you there".to_owned()));

    assert_eq!(world.ask(Request::Edit { conversation: key("u7002"), message_id: "101".into(), text: "See you at seven".into() }), Response::Ok);
    let edited = world.message("u7002", "101").unwrap();
    assert!(edited.edited && edited.text == "See you at seven");

    // A reaction shows at once and is what Telegram then says; a refused one is taken back.
    assert_eq!(world.ask(Request::React { conversation: key("u7002"), message_id: "12".into(), emoji: "👍".into(), on: true }), Response::Ok);
    let ours = world.message("u7002", "12").unwrap().reactions;
    assert_eq!((ours.len(), ours[0].emoji.as_str(), ours[0].count, ours[0].mine), (1, "👍", 1, true));
    let refused = world.ask(Request::React { conversation: key("u7002"), message_id: "12".into(), emoji: "🚫".into(), on: true });
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "That reaction isn't allowed here.".into() });
    assert!(world.message("u7002", "12").unwrap().reactions.iter().all(|reaction| reaction.emoji != "🚫"));

    assert_eq!(world.ask(Request::Delete { conversation: key("u7002"), message_id: "101".into() }), Response::Ok);
    assert!(world.message("u7002", "101").is_none());
    assert!(world.calls().contains(&"DeleteMessages [101] revoke true".to_owned()));

    // A refusal is the reason on the message, in words.
    world.send("u7002", "5002", "refuse me", None);
    let failed = world.until("the refusal", || {
        world.messages("u7002").into_iter().find(|message| matches!(message.state, SendState::Failed { .. }))
    });
    assert_eq!(failed.state, SendState::Failed { reason: "You can't write in this chat.".into() });

    // A person found by search can be written to without telling Telegram anything first.
    world.ask(Request::People { network: Some(Network::Telegram), relation: None, query: Some("wren".into()), offset: 0, limit: 20 });
    world.until("the search to reach Telegram", || world.calls().contains(&"contacts.Search wren".to_owned()).then_some(()));
    let started = world.until("the direct chat", || match world.ask(Request::StartDirect { person: key("u7010") }) {
        Response::Conversation { conversation } => Some(conversation),
        _ => None,
    });
    assert_eq!((started.key.id.as_str(), started.title.as_str(), started.kind), ("u7010", "Wren Halloran", ConversationKind::Direct));
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7002"), space: None }) else {
        panic!("no profile");
    };
    assert_eq!(profile.bio, "Maps, mostly.");
    let Response::Members { members, sections, .. } = world.ask(Request::Members { conversation: key("g8001"), offset: 0, limit: 50 }) else {
        panic!("no members");
    };
    assert_eq!((sections[0].count, members.len(), members[1].name.as_str(), members[1].relation), (3, 3, "Odalys Brandt", Relation::Contact));
}

#[test]
fn a_flood_wait_is_slept_in_full_and_a_long_one_is_told_not_retried() {
    let world = World::signed_in();
    world.connected();
    world.steer("flood-send 2");
    sleep(Duration::from_millis(200));
    let began = Instant::now();
    world.send("u7002", "6001", "slowly", None);
    world.until("the message to go after the wait", || world.messages("u7002").into_iter().find(|message| message.text == "slowly" && message.state == SendState::Sent));
    assert!(began.elapsed() >= Duration::from_secs(2), "sent after {:?}", began.elapsed());
    let calls = world.calls();
    let at = |line: &str| calls.iter().position(|call| call == line).unwrap_or_else(|| panic!("{line} in {calls:#?}"));
    assert!(at("SendMessage u7002 refused 2") < at("SendMessage u7002 slowly"));

    // Ten minutes is not waited out behind the user's back: they are told, and nothing is retried.
    world.steer("flood-send 600");
    sleep(Duration::from_millis(200));
    world.send("u7002", "6002", "later", None);
    let failed = world.until("the wait to be told", || world.messages("u7002").into_iter().find(|message| message.text == "later" && message.state != SendState::Pending));
    assert_eq!(failed.state, SendState::Failed { reason: "Telegram asked for a pause. Try again in 10 minutes.".into() });
    sleep(Duration::from_millis(300));
    assert_eq!(world.called("SendMessage u7002 later"), 0);
}

#[test]
fn a_channel_shows_its_sponsored_message_when_opened_and_says_so() {
    let world = World::signed_in();
    world.connected();
    let ad = world.until("the sponsored message", || world.messages("c9001").into_iter().find(|message| message.id == "adad01"));
    assert_eq!(ad.card.as_ref().map(|card| (card.title.as_str(), card.footer.as_str())), Some(("Sponsored", "Larch Boots GmbH")));
    assert_eq!((ad.sender.name.as_str(), ad.text.as_str()), ("Larch Boots", "Resoled by hand in Innsbruck.\nOpen site"));
    assert_eq!(ranges(&ad), vec![("Open site", &Style::Link { url: "https://example.org/boots".into() })]);
    world.until("Telegram to be told it was shown", || (world.called("ViewSponsoredMessage") == 1).then_some(()));
    // It sits at the foot of the channel, and the channel's row still previews its own post.
    let messages = world.messages("c9001");
    assert_eq!(messages.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), vec!["5", "adad01"]);
    let row = world.until("the row", || world.conversation("c9001").filter(|row| row.preview.starts_with("Snow line")));
    assert_eq!(row.unread, 1);
    // A direct chat has none.
    world.until("history", || world.message("u7002", "12"));
    assert_eq!(world.called("GetSponsoredMessages"), 1);
}

#[test]
fn a_session_ended_elsewhere_keeps_what_was_read_and_signing_out_ends_it_on_telegram() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    world.steer("revoke");
    let expired = world.until("the account to expire", || world.status().0.into_iter().find(|account| account.link == Link::Expired));
    assert_eq!(expired.detail, "Telegram ended this session.");
    assert!(world.message("u7002", "12").is_some());
    let refused = world.ask(Request::Send { conversation: key("u7002"), client_id: "7001".into(), text: "hello?".into(), reply_to: None, with: Default::default() });
    assert!(matches!(refused, Response::Error { code: ErrorCode::Offline, .. }), "{refused:?}");

    let world = World::signed_in();
    world.connected();
    world.ask(Request::SignOut { account: account() });
    assert_eq!(world.called("LogOut"), 1);
    assert!(world.conversation("u7002").is_none());
}

#[test]
fn a_qr_code_is_shown_replaced_when_it_runs_out_and_scanning_it_signs_in() {
    // Codes that last four seconds, so one is replaced while the test watches.
    let world = World::start(None, "4");
    let (_, seq) = world.status();
    assert!(matches!(world.sign_in(None), SignInStep::Qr { .. }));
    let (first, expires) = world.code_shown();
    assert!(first.starts_with("tg://login?token=") && first.len() == "tg://login?token=".len() + 43, "{first}");
    assert!(expires > 0);
    // The next code arrives by itself, as a change, before the first stops working.
    let second = world.changed(seq, "a fresh code", |change| match change {
        Change::SignIn { step: SignInStep::Qr { link, .. }, .. } if !link.is_empty() && *link != first => Some(link.clone()),
        _ => None,
    });
    assert_ne!(first, second);

    let (_, seq) = world.status();
    world.steer_sign_in("scan");
    let done = world.changed(seq, "the scan", |change| match change {
        Change::SignIn { network: Network::Telegram, step: SignInStep::Done { account } } => Some(account.clone()),
        _ => None,
    });
    assert_eq!(done, Some(account()));
    world.connected();
    assert_eq!(world.status().0[0].name, "Mira Kessler");
    // The sign-in is over: beginning again is a new one.
    assert!(matches!(world.sign_in(None), SignInStep::Qr { .. }));
    world.ask(Request::SignInCancel { network: Network::Telegram });
}

#[test]
fn a_scanned_code_on_an_account_with_a_second_password_asks_for_it() {
    let world = World::start(None, "30");
    world.sign_in(None);
    world.code_shown();
    let (_, seq) = world.status();
    world.steer_sign_in("scan-password");
    let hint = world.changed(seq, "the password step", |change| match change {
        Change::SignIn { step: SignInStep::Password { hint, problem: None }, .. } => Some(hint.clone()),
        _ => None,
    });
    assert_eq!(hint, "the old street");
    let wrong = world.sign_in(Some(SignInInput::Password { password: "hunter2".into() }));
    assert_eq!(wrong, SignInStep::Password { hint: "the old street".into(), problem: Some(SignInProblem::PasswordInvalid) });
    let done = world.sign_in(Some(SignInInput::Password { password: "correct horse".into() }));
    assert_eq!(done, SignInStep::Done { account: Some(account()) });
    world.connected();
    // What was typed is in no log.
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("correct horse") && !said.contains("hunter2"), "{said}");
}

#[test]
fn a_phone_number_a_code_and_a_password_sign_in_and_each_mistake_says_what_it_was() {
    let world = World::start(None, "30");
    world.sign_in(None);
    assert_eq!(world.sign_in(Some(SignInInput::UsePhone)), SignInStep::Phone { problem: None });
    // Too short to be a number: Telegram is not asked.
    assert_eq!(world.sign_in(Some(SignInInput::Phone { number: "+1 555".into() })), SignInStep::Phone { problem: Some(SignInProblem::PhoneInvalid) });
    assert_eq!(world.called("SendCode"), 0);
    assert_eq!(world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0666".into() })), SignInStep::Phone { problem: Some(SignInProblem::PhoneBanned) });
    let SignInStep::Phone { problem: Some(SignInProblem::TooMany { until }) } = world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0420".into() })) else {
        panic!("no flood wait");
    };
    assert!(until > 0);

    let sent = world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0199".into() }));
    let SignInStep::Code { via: CodeVia::App, length: 5, phone, next: Some(CodeVia::Sms), resend_at, problem: None } = sent else {
        panic!("{sent:?}");
    };
    assert_eq!(phone, "+1 555 010 0199");
    assert!(resend_at > 0);
    let wrong = world.sign_in(Some(SignInInput::Code { code: "11111".into() }));
    assert!(matches!(&wrong, SignInStep::Code { problem: Some(SignInProblem::CodeInvalid), length: 5, .. }), "{wrong:?}");
    let expired = world.sign_in(Some(SignInInput::Code { code: "00000".into() }));
    assert!(matches!(&expired, SignInStep::Code { problem: Some(SignInProblem::CodeExpired), .. }), "{expired:?}");
    let again = world.sign_in(Some(SignInInput::Resend));
    assert!(matches!(&again, SignInStep::Code { via: CodeVia::Sms, next: None, resend_at: 0, problem: None, .. }), "{again:?}");

    // Back to the number and forward again; the step on show is what `Begin` answers.
    assert_eq!(world.sign_in(Some(SignInInput::Back)), SignInStep::Phone { problem: None });
    world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0199".into() }));
    assert!(matches!(world.sign_in(None), SignInStep::Code { .. }));

    let password = world.sign_in(Some(SignInInput::Code { code: "24680".into() }));
    assert_eq!(password, SignInStep::Password { hint: "the old street".into(), problem: None });
    assert_eq!(world.sign_in(Some(SignInInput::Password { password: "correct horse".into() })), SignInStep::Done { account: Some(account()) });
    world.connected();
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("24680") && !said.contains("correct horse") && !said.contains("5550100199"), "{said}");
}

#[test]
fn cancelling_keeps_nothing_and_a_build_without_an_app_id_says_so() {
    let world = World::start(None, "30");
    world.sign_in(None);
    world.code_shown();
    assert_eq!(world.ask(Request::SignInCancel { network: Network::Telegram }), Response::Ok);
    assert!(world.status().0.is_empty());
    // An answer with no sign-in to answer does not start one.
    assert!(matches!(world.sign_in(Some(SignInInput::UsePhone)), SignInStep::Failed { .. }));
    // A network that signs in through its own window has no steps.
    let slack = world.ask(Request::SignInBegin { network: Network::Slack });
    assert!(matches!(slack, Response::Error { code: ErrorCode::Unsupported, .. }), "{slack:?}");

    // Without the scripted wire this is chatd as it is built with no app id: Telegram is
    // unsupported, with the reason, and nothing of it is loaded.
    if option_env!("TINYCHAT_TELEGRAM_API_ID").is_some() {
        return;
    }
    let dir = PathBuf::from(format!("/tmp/tinychat-tg-{}-plain", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_chatd")).env("TINYCHAT_DIR", &dir).env_remove("TINYCHAT_TELEGRAM_FAKE").stderr(Stdio::null()).spawn().unwrap();
    let plain = World { child, dir };
    plain.until("the socket", || ChatStream::connect(&plain.dir.join("chat.sock")).ok().map(drop));
    let answer = plain.ask(Request::SignInBegin { network: Network::Telegram });
    let Response::Error { code: ErrorCode::Unsupported, detail } = answer else {
        panic!("{answer:?}");
    };
    assert!(detail.contains("without a Telegram app id"), "{detail}");
    let Response::Status { networks, .. } = plain.ask(Request::Status) else {
        panic!("no status");
    };
    assert!(networks.iter().any(|network| network.network == Network::Telegram && network.link == Link::SignedOut));
}
