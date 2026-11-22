//! The Telegram adapter through chatd's real socket, with a scripted wire where Telegram would
//! be (`adapters/telegram/fake.rs`): the account, its people and its messages are invented there,
//! and each test steers it by appending a line to a file in its scratch directory.
//!
//! What this proves is everything above the wire: which requests the adapter makes and how it
//! maps what comes back into the contract, and the sign-in steps. It proves nothing about the
//! MTProto library underneath, which no test can reach without a registered app id and a real
//! account.

use protocol::{
    read_frame, write_frame, AccountKey, Change, CodeVia, Conversation, ConversationKind, ErrorCode, Key, Link,
    EmojiScope, MediaKind, MentionKind, Message, MessageKind, Network, Page, Presence, RelateAction, Relation, Request,
    Response,
    SendState, SignInInput, SignInProblem, SignInStep, Style, Watching, Shelf, Special, Mark, ReactionSet,
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

/// Where the fake Telegram's clock starts: yesterday's midnight UTC, so its chats are always recent
/// enough for the Inbox (it drops what nobody touched for thirty days) and `t0() + 300`, when its
/// users stop being online, has always passed. One value for the whole process; chatd gets it by env.
fn t0() -> i64 {
    static T0: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *T0.get_or_init(|| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        now - now % 86_400 - 86_400
    })
}

fn key(id: &str) -> Key {
    Key { network: Network::Telegram, account: "7001".into(), id: id.into() }
}

fn account() -> AccountKey {
    AccountKey { network: Network::Telegram, account: "7001".into() }
}

/// `CHATD_TEST_BIN` names a copy of the binary, for a target directory other builds also write to.
fn chatd_bin() -> String {
    std::env::var("CHATD_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_chatd").to_owned())
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

    /// The same account with Telegram Premium, which is what custom emoji may be sent as.
    fn premium() -> World {
        World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_PREMIUM", "1")])
    }

    /// Signed in to an account with this many more chats, each with a person of its own.
    fn crowded(many: u32) -> World {
        World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_MANY", &many.to_string())])
    }

    /// `many` more chats, with every chats page after the first held at the wire until the test
    /// lets them go: the pages then land apart, as they do on a real network.
    fn held(many: u32) -> World {
        World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_MANY", &many.to_string()), ("TINYCLIENT_TELEGRAM_HOLD", "1")])
    }

    /// `many` more chats, and the group's dialog ask held. The store here already read the
    /// group further than Telegram now says, so the writing of that row asks for its dialog
    /// again and stalls mid-page until the test lets the ask through.
    fn stalled(many: u32) -> World {
        World::seed(
            Some(SESSION),
            "30",
            &[("TINYCLIENT_TELEGRAM_MANY", &many.to_string()), ("TINYCLIENT_TELEGRAM_PEER_HOLD", "1")],
            |store| {
                let far = key("g8001");
                let row = chatd::store::ConversationRow::new("g8001", ConversationKind::Group, "Trail crew");
                store.upsert_conversation(&far.account(), &row).unwrap();
                // The read has to be written fresh: a count that changes nothing is no write.
                store.set_counts(&far, 0, 0, Some("25"), 1).unwrap();
            },
        )
    }

    /// `session` is the stored sign-in, if any; `qr_seconds` how long the fake's QR codes last.
    fn start(session: Option<&str>, qr_seconds: &str) -> World {
        World::with(session, qr_seconds, &[])
    }

    fn with(session: Option<&str>, qr_seconds: &str, env: &[(&str, &str)]) -> World {
        World::seed(session, qr_seconds, env, |_| {})
    }

    /// `seed` writes the store of a run that is not the first, as the boot of an account
    /// already signed in carries one.
    fn seed(
        session: Option<&str>,
        qr_seconds: &str,
        env: &[(&str, &str)],
        seed: impl FnOnce(&mut chatd::store::Store),
    ) -> World {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // On Windows "/tmp" has no drive; chatd takes only absolute paths.
        let dir = std::path::absolute(format!("/tmp/tinyclient-tg-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        seed(&mut chatd::store::Store::open(&dir.join("chat.sqlite")).unwrap());
        let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
        let mut command = Command::new(chatd_bin());
        command
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_TELEGRAM_FAKE", qr_seconds)
            .env("TINYCLIENT_TELEGRAM_T0", t0().to_string())
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .env_remove("TINYCLIENT_TELEGRAM_MANY")
            .env_remove("TINYCLIENT_TELEGRAM_PREMIUM")
            .stderr(Stdio::from(log));
        for (name, value) in env {
            command.env(name, value);
        }
        if let Some(session) = session {
            command.env("TINYCLIENT_SESSION_TELEGRAM", session);
        }
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket");
            sleep(Duration::from_millis(10));
        }
        World { child, dir }
    }

    /// Starts chatd again on the same store. The scripted wire starts fresh; lines already
    /// taken stay taken, so a restart does not replay them.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.dir.join("chat.sock"));
        let log = std::fs::File::create(self.dir.join("stderr.log")).unwrap();
        let child = Command::new(chatd_bin())
            .env("TINYCLIENT_DIR", &self.dir)
            .env("TINYCLIENT_TELEGRAM_FAKE", "30")
            .env("TINYCLIENT_TELEGRAM_T0", t0().to_string())
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env("TINYCLIENT_SESSION_TELEGRAM", SESSION)
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        self.child = child;
        let deadline = Instant::now() + Duration::from_secs(10);
        while ChatStream::connect(&self.dir.join("chat.sock")).is_err() {
            assert!(Instant::now() < deadline, "chatd did not open its socket after a restart");
            sleep(Duration::from_millis(10));
        }
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
        self.until("Telegram to connect", || self.status().0.iter().any(|account| account.link == Link::Connected).then_some(()));
        self.until("the chats", || self.conversation("u7003").map(drop));
    }

    fn conversation(&self, id: &str) -> Option<Conversation> {
        match self.ask(Request::Conversation { conversation: key(id) }) {
            Response::Conversation { conversation } => Some(conversation),
            _ => None,
        }
    }

    /// The Telegram home: the main list, a folder of it, or the archive.
    fn home(&self, folder: Option<&str>, shelf: Shelf) -> Vec<Conversation> {
        let asked = Request::Home { network: Network::Telegram, account: Some(account()), folder: folder.map(str::to_owned), shelf, offset: 0, limit: 200 };
        match self.ask(asked) {
            Response::Conversations { conversations, .. } => conversations,
            other => panic!("{other:?}"),
        }
    }

    fn watch(&self, watching: Watching) {
        self.ask(Request::Watch(watching));
    }

    fn rows_on_screen(&self, ids: &[&str]) {
        self.watch(Watching { rows: ids.iter().map(|id| key(id)).collect(), ..Watching::default() });
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
fn the_chats_are_one_list_with_no_space_each_filed_in_every_folder_it_matches() {
    let world = World::signed_in();
    world.connected();
    let (accounts, _) = world.status();
    assert_eq!((accounts[0].name.as_str(), accounts[0].me.as_str(), accounts[0].key.account.as_str()), ("Mira Kessler", "u7001", "7001"));
    // What Telegram lets this account send, as its session says.
    assert!(accounts[0].can.spoiler && accounts[0].can.silent && accounts[0].can.schedule && accounts[0].can.gifs);
    assert_eq!((accounts[0].can.max_files, accounts[0].can.caption_limit), (10, 1_024));
    // Enter asks Telegram itself for the open conversation's words.
    assert!(accounts[0].can.search, "Telegram searches one conversation on the network");
    // No invented Space: the chats are the Telegram home.
    let Response::Spaces { spaces } = world.ask(Request::Spaces { network: Some(Network::Telegram) }) else {
        panic!("no spaces");
    };
    assert!(spaces.is_empty(), "{spaces:?}");

    // Telegram's pin first, then by recency; the archived chat is on its own shelf.
    let rows = world.home(None, Shelf::Main);
    let listed: Vec<(&str, Option<i32>)> = rows.iter().map(|row| (row.key.id.as_str(), row.pin)).collect();
    assert_eq!(&listed[..3], &[("u7002", Some(0)), ("c9002", None), ("g8001", None)]);
    assert!(rows.iter().all(|row| row.space.is_none() && row.key.id != "u7003"));
    let row = |id: &str| rows.iter().find(|row| row.key.id == id).unwrap();
    assert_eq!((row("u7002").title.as_str(), row("u7002").unread, row("u7002").last_read.as_deref()), ("Odalys Brandt", 1, Some("11")));
    assert_eq!(row("u7002").preview, "Good. Trailhead at seven.");
    assert_eq!((row("u7002").peer.as_deref(), row("u7002").call), (Some("u7002"), protocol::CallSupport::Handoff));
    // Saved Messages: the user's own notes, no presence, no call.
    assert_eq!((row("u7001").title.as_str(), row("u7001").special, row("u7001").peer.as_deref(), row("u7001").call), ("Saved Messages", Special::Saved, None, protocol::CallSupport::None));
    // The login-code chat: Telegram's own, verified, read-only, no Call button.
    let service = row("u777000");
    assert_eq!((service.special, service.mark, service.read_only, service.call), (Special::Service, Some(Mark::Verified), true, protocol::CallSupport::None));
    // Marks from Telegram's flags; a bot is a bot.
    assert_eq!((row("u7005").mark, row("c9001").mark), (Some(Mark::Scam), Some(Mark::Verified)));
    // The impostor Telegram flags: the same red word the row and the header draw.
    assert_eq!(row("u7006").mark, Some(Mark::Fake));
    assert!(row("u7004").bot && row("u7004").call == protocol::CallSupport::None);
    // The channel is muted on Telegram and only its owners post in it.
    assert!(row("c9001").muted && row("c9001").read_only && !row("g8001").muted);
    let archive = world.home(None, Shelf::Archive);
    assert_eq!(archive.iter().map(|row| (row.key.id.as_str(), row.archived)).collect::<Vec<_>>(), [("u7003", true)]);

    // Folders are filters: the group is in Work and in Family; Family's own pin comes first.
    let Response::Folders { folders } = world.ask(Request::Folders { network: Network::Telegram }) else {
        panic!("no folders");
    };
    assert_eq!(folders.iter().map(|folder| (folder.key.id.as_str(), folder.name.as_str())).collect::<Vec<_>>(), [("2", "Work"), ("3", "Family")]);
    assert_eq!(row("g8001").folders, ["2", "3"]);
    let ids = |rows: Vec<Conversation>| rows.into_iter().map(|row| row.key.id).collect::<Vec<_>>();
    assert_eq!(ids(world.home(Some("2"), Shelf::Main)), ["c9002", "g8001"]);
    assert_eq!(ids(world.home(Some("3"), Shelf::Main)), ["u7002", "g8001"]);

    // The Inbox has people, bots, groups and the service chat; the channel only if starred,
    // the archived chat only if it mentions me.
    let inbox = || match world.ask(Request::Inbox { network: Some(Network::Telegram), filter: Default::default(), offset: 0, limit: 50 }) {
        Response::Conversations { conversations, .. } => conversations.into_iter().map(|row| row.key.id).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let held = inbox();
    assert!(held.contains(&"u777000".to_owned()) && held.contains(&"g8001".to_owned()) && held.contains(&"u7004".to_owned()));
    assert!(!held.contains(&"c9001".to_owned()) && !held.contains(&"u7003".to_owned()), "{held:?}");
    world.ask(Request::Star { conversation: key("c9001"), starred: true });
    assert!(inbox().contains(&"c9001".to_owned()));

    // Connecting read the chat list once (one page here, and the archive), the folders once,
    // and fetched no picture: pictures are for rows on screen.
    let calls = world.calls();
    let count = |start: &str| calls.iter().filter(|call| call.starts_with(start)).count();
    assert_eq!((count("GetDialogs 0"), count("GetDialogs 1"), count("GetDialogFilters"), count("GetContacts")), (1, 1, 1, 1), "{calls:#?}");
    assert_eq!(count("GetFile"), 0, "{calls:#?}");
    world.rows_on_screen(&["u7002", "u7001"]);
    world.until("the picture of the row on screen", || (world.called("GetFile avatar 5002") > 0).then_some(()));
    // Shown again, it is not asked again.
    world.rows_on_screen(&["u7002"]);
    sleep(Duration::from_millis(300));
    assert_eq!(world.called("GetFile avatar 5002 at 0"), 1, "{:#?}", world.calls());

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
fn a_chip_counts_the_unread_unmuted_chats_of_its_folder_and_one_in_two_folders_counts_in_both() {
    let world = World::signed_in();
    world.connected();
    let Response::Folders { folders } = world.ask(Request::Folders { network: Network::Telegram }) else {
        panic!("no folders");
    };
    let count = |id: &str| folders.iter().find(|folder| folder.key.id == id).map(|folder| (folder.unread, folder.mentions));
    // Work holds the forum (one unread) and the group (read); Family holds Odalys (one unread)
    // and the group. A read chat counts on no chip.
    assert_eq!(count("2"), Some((1, 0)), "{folders:?}");
    assert_eq!(count("3"), Some((1, 0)), "{folders:?}");
    // A message that names me in the group, which sits in both folders, counts on both chips.
    world.steer("mention group");
    let folders = world.until("the mention on both chips", || {
        let Response::Folders { folders } = world.ask(Request::Folders { network: Network::Telegram }) else {
            panic!("no folders");
        };
        let count = |id: &str| folders.iter().find(|folder| folder.key.id == id).map(|folder| (folder.unread, folder.mentions));
        (count("2") == Some((2, 1)) && count("3") == Some((2, 1))).then_some(folders)
    });
    assert_eq!(folders.iter().map(|folder| folder.key.id.as_str()).collect::<Vec<_>>(), ["2", "3"], "{folders:?}");
}

#[test]
fn an_archived_chat_stays_out_of_the_home_and_inbox_until_it_mentions_me() {
    let world = World::signed_in();
    world.connected();
    let home_ids = || -> Vec<String> { world.home(None, Shelf::Main).into_iter().map(|row| row.key.id).collect() };
    let inbox_ids = || -> Vec<String> {
        match world.ask(Request::Inbox { network: Some(Network::Telegram), filter: Default::default(), offset: 0, limit: 50 }) {
            Response::Conversations { conversations, .. } => conversations.into_iter().map(|row| row.key.id).collect(),
            other => panic!("{other:?}"),
        }
    };
    // Tobin is archived: not in the home list, not in the Inbox, not on the rail's count.
    assert!(!home_ids().contains(&"u7003".to_owned()));
    assert!(!inbox_ids().contains(&"u7003".to_owned()), "{:?}", inbox_ids());
    assert_eq!(world.status().0[0].attention, 1, "{:#?}", world.status().0);
    // His message names me: the archived chat reaches the Inbox and raises the count,
    // and the home list still keeps the archive out.
    world.steer("mention");
    world.until("the mention on the archived row", || world.conversation("u7003").filter(|row| row.mentions > 0).map(drop));
    assert!(inbox_ids().contains(&"u7003".to_owned()), "{:?}", inbox_ids());
    assert!(!home_ids().contains(&"u7003".to_owned()), "{:?}", home_ids());
    assert_eq!(world.status().0[0].attention, 2, "{:#?}", world.status().0);
}

#[test]
fn a_folder_that_excludes_read_chats_keeps_the_open_one_until_it_is_left() {
    let world = World::signed_in();
    world.connected();
    let ids = |rows: Vec<Conversation>| rows.into_iter().map(|row| row.key.id).collect::<Vec<_>>();
    // The Work folder now takes its chats by kind and leaves read ones out: the read group
    // goes, the unread forum stays.
    world.steer("exclude-read");
    world.until("the read group out of Work", || {
        (ids(world.home(Some("2"), Shelf::Main)) == ["c9002".to_owned()]).then_some(())
    });
    // The forum is open while it is read: it keeps its place in Work.
    world.watch(Watching { conversation: Some(key("c9002")), ..Watching::default() });
    world.steer("read-forum");
    world.until("the forum read", || world.conversation("c9002").filter(|row| row.unread == 0).map(drop));
    assert_eq!(ids(world.home(Some("2"), Shelf::Main)), ["c9002".to_owned()], "the open chat stays put while it is read");
    // Leaving it files it again by the live facts: a read chat is out of Work.
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("the forum out of Work once left", || {
        ids(world.home(Some("2"), Shelf::Main)).is_empty().then_some(())
    });
}

#[test]
fn a_muted_channel_draws_quiet_and_never_raises_the_rail() {
    let world = World::signed_in();
    world.connected();
    // The channel is muted on Telegram, and one of its unread posts names me. The row
    // carries what the list needs to draw it dimmed with a quiet count and the quiet
    // mention pill (polish/03 §6); the rail hears nothing of any of it.
    world.steer("mention channel");
    world.home(None, Shelf::Main);
    let row = world.until("the channel's mention to land", || world.conversation("c9001").filter(|row| row.mentions == 1));
    assert_eq!((row.unread, row.muted, row.read_only), (1, true, true), "{row:?}");
    let (accounts, _) = world.status();
    let mira = accounts.iter().find(|account| account.key.network == Network::Telegram).unwrap();
    assert_eq!(mira.attention, 1, "only Odalys's unread needs me: the muted channel never raises the rail");
}

#[test]
fn a_forum_opens_on_the_newest_unread_topic_else_general() {
    let world = World::signed_in();
    world.connected();
    // Opening the forum reads its topics and lands on the newest unread: Jigs here, active
    // after the unread Tools, General read all along (polish/03 §2, story T8).
    let landing = || Request::Topics {
        forum: key("c9002"),
        offset: 0,
        limit: 1,
        landing: true,
        sort: protocol::TopicSort::Activity,
        tag: None,
        query: None,
    };
    let Response::Conversations { conversations, .. } = world.ask(landing()) else {
        panic!("no landing");
    };
    assert_eq!(conversations.len(), 1, "{conversations:?}");
    assert_eq!((conversations[0].key.id.as_str(), conversations[0].title.as_str(), conversations[0].parent.as_deref()), ("c9002_55", "Jigs", Some("c9002")));
    // What the topics drill lists: the network's pins first (none is pinned), then the
    // activity, a count on every row.
    let drill = Request::Topics {
        forum: key("c9002"),
        offset: 0,
        limit: 10,
        landing: false,
        sort: protocol::TopicSort::Activity,
        tag: None,
        query: None,
    };
    let Response::Conversations { conversations: topics, total, .. } = world.ask(drill) else {
        panic!("no topics");
    };
    assert_eq!((total, topics.len()), (3, 3), "{topics:?}");
    assert_eq!(topics.iter().map(|topic| topic.key.id.as_str()).collect::<Vec<_>>(), ["c9002_55", "c9002_31", "c9002_1"]);
    assert_eq!((topics[0].unread, topics[1].unread, topics[2].unread), (2, 1, 0));

    // Read both unread topics, as opening them and reading does: the landing is General.
    for (topic, read) in [("c9002_55", "55"), ("c9002_31", "41")] {
        world.ask(Request::MarkRead { conversation: key(topic), message_id: read.into() });
    }
    world.until("both reads told to Telegram", || {
        (world.called("ReadDiscussion c9002_55 55") > 0 && world.called("ReadDiscussion c9002_31 41") > 0).then_some(())
    });
    let Response::Conversations { conversations, .. } = world.ask(landing()) else {
        panic!("no landing");
    };
    assert_eq!(conversations[0].key.id, "c9002_1", "with nothing unread the landing is General");
}

#[test]
fn chats_past_the_newest_500_load_as_the_list_scrolls_with_their_pictures() {
    let world = World::crowded(650);
    world.connected();
    // 8 chats of the account's own and 650 more: five pages of a hundred are read at connect.
    let known = |at: u32| world.conversation(&format!("u{}", 100_000 + at)).is_some();
    world.until("the newest 500", || known(491).then_some(()));
    assert!(!known(492), "chat 501 is not read at connect");
    assert_eq!(world.called("GetDialogs 0"), 5, "{:#?}", world.calls());
    // Scrolled to near the end of what is loaded: the next page, once, and the rows on screen
    // get their pictures.
    world.rows_on_screen(&["u100480", "u100491"]);
    world.until("the next page", || known(591).then_some(()));
    assert_eq!(world.called("GetDialogs 0"), 6);
    world.until("the picture of a row past 400", || (world.called("GetFile avatar 5100491") > 0).then_some(()));
    // A row in the middle of what is loaded asks for nothing more.
    world.rows_on_screen(&["u100300"]);
    sleep(Duration::from_millis(300));
    assert_eq!(world.called("GetDialogs 0"), 6);
}

#[test]
fn rows_past_the_first_page_get_their_pictures_when_watched_one_file_each_at_most_four_in_flight() {
    let world = World::crowded(650);
    world.connected();
    let known = |at: u32| world.conversation(&format!("u{}", 100_000 + at)).is_some();
    world.until("the newest 500", || known(491).then_some(()));
    assert_eq!(world.called("GetDialogs 0"), 5, "{:#?}", world.calls());
    // The shell scrolls to the rows past the first page and says so: their pictures are
    // asked for, each one file, and no chats are read again for any of them.
    let rows: Vec<String> = (150..200).map(|at| format!("u{}", 100_000 + at)).collect();
    let named: Vec<&str> = rows.iter().map(String::as_str).collect();
    world.rows_on_screen(&named);
    let last = "GetFile avatar 5100199";
    world.within(Duration::from_secs(45), "the last row's picture to be asked for", || {
        (world.called(last) > 0).then_some(())
    });
    for at in 150..200 {
        let asked = format!("GetFile avatar {} at 0", 5_100_000 + at);
        assert_eq!(world.called(&asked), 1, "row {at}: {:#?}", world.calls());
    }
    assert_eq!(world.called("GetDialogs 0"), 5, "an avatar ask is one file, not a dialogs re-read");
    // However the fetching runs, at most four of them are in flight at once.
    world.steer("file-count");
    let at_once = world
        .calls()
        .iter()
        .find_map(|call| call.strip_prefix("file at once ")?.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(at_once <= 4, "{at_once} file asks were in flight at once: {:#?}", world.calls());
}

#[test]
fn a_row_scrolled_away_before_its_picture_waits_unfetched_and_is_fetched_once_it_is_back() {
    let world = World::crowded(60);
    world.connected();
    // Two rows on screen: both their pictures go in line, and the first is fetched.
    world.rows_on_screen(&["u100000", "u100001"]);
    world.until("the first row's picture", || (world.called("GetFile avatar 5100000") > 0).then_some(()));
    // Scrolled away before its picture's turn came: the third row takes its place, and the
    // second row's picture waits, unfetched, however long the fetching goes on.
    world.rows_on_screen(&["u100002"]);
    world.until("the third row's picture", || (world.called("GetFile avatar 5100002") > 0).then_some(()));
    sleep(Duration::from_millis(400));
    assert_eq!(world.called("GetFile avatar 5100001"), 0, "{:#?}", world.calls());
    // Back on screen: it goes back in line and is fetched now, once.
    world.rows_on_screen(&["u100001"]);
    world.until("the second row's picture once its row came back", || {
        (world.called("GetFile avatar 5100001") > 0).then_some(())
    });
    assert_eq!(world.called("GetFile avatar 5100001"), 1, "each picture once");
    // Returning a parked picture must leave room for a picture never queued before.
    world.rows_on_screen(&["u100003"]);
    world.until("a new row's picture after the parked one returned", || {
        (world.called("GetFile avatar 5100003") > 0).then_some(())
    });
}

/// The photo `photo-pages` writes into Trail crew: its thumbnail is one piece.
const CREW_THUMB: &str = "GetFile photo 6020 m";

/// An ask for a piece of the thumbnail of a photo post `photo-pages` wrote into Field Notes
/// (`p7100m` up). Each is two pieces.
fn channel_thumb(call: &str) -> bool {
    call.starts_with("GetFile photo 71") && call.contains(" m at ")
}

/// The shell drawing these files on screen asks for them, as `fetch_wanted` does.
fn on_screen(world: &World, ids: &[&str]) {
    let ids = ids.iter().map(|id| (*id).to_owned()).collect();
    match world.ask(Request::Fetch { account: account(), ids, size: Some(320), retry: false, span: None }) {
        Response::Media { .. } => {}
        other => panic!("{other:?}"),
    }
}

/// How many pieces of the channel's thumbnails Telegram was asked for from the `since`th call
/// to the first that starts with `line`, and how many after it.
fn channel_thumbs_around(world: &World, since: usize, line: &str) -> (usize, usize) {
    let calls = world.calls();
    let at = calls.iter().position(|call| call.starts_with(line)).unwrap_or_else(|| panic!("{line} in {calls:#?}"));
    let count = |calls: &[String]| calls.iter().filter(|call| channel_thumb(call)).count();
    (count(&calls[since.min(at)..at]), count(&calls[at + 1..]))
}

/// At most what was being fetched when a picture was asked for, and the one after it if the
/// ask landed just as that began: two thumbnails of two pieces.
const IN_FLIGHT: usize = 4;

/// Waits until every piece of the `many` channel thumbnails has been asked for, so the order
/// they went in is whole.
fn all_channel_thumbs(world: &World, many: usize) {
    world.within(Duration::from_secs(60), "every channel thumbnail", || {
        (world.calls().iter().filter(|call| channel_thumb(call)).count() >= 2 * many).then_some(())
    });
}

/// Opens Field Notes with `many` photo posts no thumbnail of which is on disk, and waits for
/// the first of them to be fetched: the rest wait their turn behind it.
fn channel_fetching(many: u32) -> World {
    let world = World::signed_in();
    world.connected();
    world.steer(&format!("photo-pages {many}"));
    world.messages("c9001");
    world.until("the channel's first thumbnail", || world.calls().iter().any(|call| channel_thumb(call)).then_some(()));
    world
}

#[test]
fn the_pictures_on_screen_are_fetched_before_those_still_waiting_from_a_channel_left_or_off_screen() {
    let world = channel_fetching(12);
    // Scrolled up to the oldest post, whose thumbnail the page named last: it goes next.
    let scrolled = world.calls().len();
    on_screen(&world, &["p7100m"]);
    world.within(Duration::from_secs(60), "the oldest post's thumbnail", || (world.called("GetFile photo 7100 m") > 0).then_some(()));

    // Trail crew opened while the channel's thumbnails still wait: the photo it shows goes
    // ahead of them, and three asks for it are one download.
    world.messages("g8001");
    world.until("the group's photo", || world.message("g8001", "90"));
    let opened = world.calls().len();
    for _ in 0..3 {
        on_screen(&world, &["p6020m"]);
    }
    world.within(Duration::from_secs(60), "the group's thumbnail", || (world.called(CREW_THUMB) > 0).then_some(()));

    // Every thumbnail still comes in the end, each once, and the ones on screen came next.
    all_channel_thumbs(&world, 12);
    let (waited, after) = channel_thumbs_around(&world, scrolled, "GetFile photo 7100 m at 0");
    assert!(waited <= IN_FLIGHT && after > 0, "the oldest post's thumbnail waited behind {waited} pieces: {:#?}", world.calls());
    let (waited, after) = channel_thumbs_around(&world, opened, CREW_THUMB);
    assert!(waited <= IN_FLIGHT && after > 0, "the group's thumbnail waited behind {waited} pieces: {:#?}", world.calls());
    for at in 0..12 {
        assert_eq!(world.called(&format!("GetFile photo {} m at 0", 7_100 + at)), 1, "{:#?}", world.calls());
    }
    assert_eq!(world.called(CREW_THUMB), 1, "{:#?}", world.calls());
    let landed = world.message("g8001", "90").and_then(|message| message.attachments[0].thumb.clone()).and_then(|thumb| thumb.path);
    assert_eq!(std::fs::metadata(landed.expect("the group's thumbnail on disk")).unwrap().len(), include_bytes!("../src/adapters/telegram/fake_crew_m.jpg").len() as u64);
}

#[test]
fn a_picture_asked_for_before_its_page_is_read_goes_ahead_once_the_page_names_it() {
    let world = channel_fetching(12);
    // The group's photo is on screen from what the store had, before its page is read again:
    // nothing here knows yet where it is.
    on_screen(&world, &["p6020m"]);
    sleep(Duration::from_millis(300));
    assert_eq!(world.called(CREW_THUMB), 0);
    world.messages("g8001");
    world.within(Duration::from_secs(60), "the group's thumbnail", || (world.called(CREW_THUMB) > 0).then_some(()));
    all_channel_thumbs(&world, 12);
    // From the moment its page was read, it waited only for what was being fetched.
    let calls = world.calls();
    let read = calls.iter().position(|call| call.starts_with("GetHistory g8001")).unwrap_or_else(|| panic!("{calls:#?}"));
    let (waited, after) = channel_thumbs_around(&world, read, CREW_THUMB);
    assert!(waited <= IN_FLIGHT && after > 0, "the group's thumbnail waited behind {waited} pieces: {calls:#?}");
}

#[test]
fn the_original_the_viewer_waits_on_goes_before_the_pictures_on_screen() {
    let world = channel_fetching(12);
    world.messages("g8001");
    world.until("the group's photo", || world.message("g8001", "90"));
    // Telegram asks for a pause on the next piece: nothing is fetched while both are asked for,
    // so their order is the line's and not the order they arrived in.
    world.steer("flood-file 2");
    world.until("Telegram to ask for a pause", || world.calls().iter().any(|call| call.ends_with(" refused 2")).then_some(()));
    let paused = Instant::now();
    // The group's photo on screen, then the viewer opened on the channel's newest post.
    on_screen(&world, &["p6020m"]);
    on_screen(&world, &["p7111y"]);
    assert!(paused.elapsed() < Duration::from_millis(1_500), "asked for after the pause: {:?}", paused.elapsed());
    world.within(Duration::from_secs(60), "the group's thumbnail", || (world.called(CREW_THUMB) > 0).then_some(()));
    all_channel_thumbs(&world, 12);
    // After the file that was paused: the original, in two pieces, then the group's photo, then
    // the rest of the channel.
    let calls = world.calls();
    let refused = calls.iter().position(|call| call.ends_with(" refused 2")).unwrap();
    let paused_file = calls[refused].trim_end_matches(" refused 2").to_owned() + " at ";
    let next: Vec<&str> = calls[refused + 1..].iter().map(String::as_str).filter(|call| call.starts_with("GetFile") && !call.starts_with(&paused_file)).take(4).collect();
    assert_eq!(&next[..3], ["GetFile photo 7111 y at 0", "GetFile photo 7111 y at 262144", "GetFile photo 6020 m at 0"], "{calls:#?}");
    assert!(channel_thumb(next[3]), "{calls:#?}");
}

#[test]
fn a_flood_wait_on_a_picture_on_screen_holds_every_file_until_it_has_passed() {
    let world = World::signed_in();
    world.connected();
    world.steer("photo-pages 3");
    world.steer("flood-file 2");
    world.messages("g8001");
    world.until("the group's photo", || world.message("g8001", "90"));
    on_screen(&world, &["p6020m"]);
    world.until("Telegram to ask for a pause", || (world.called("GetFile photo 6020 m refused 2") == 1).then_some(()));
    let refused = Instant::now();
    // The channel opened during the pause: its thumbnails wait for it to pass too.
    world.messages("c9001");
    world.until("the group's thumbnail after the pause", || (world.called("GetFile photo 6020 m at 0") == 1).then_some(()));
    assert!(refused.elapsed() >= Duration::from_millis(1_700), "asked again after {:?}", refused.elapsed());
    let calls = world.calls();
    let at = |line: &str| calls.iter().position(|call| call == line).unwrap_or_else(|| panic!("{line} in {calls:#?}"));
    assert!(calls[at("GetFile photo 6020 m refused 2") + 1..at("GetFile photo 6020 m at 0")].iter().all(|call| !call.starts_with("GetFile")), "{calls:#?}");
    world.until("the channel's thumbnails after it", || world.calls().iter().any(|call| channel_thumb(call)).then_some(()));
}

#[test]
fn a_chats_list_paints_what_it_has_while_the_pages_arrive() {
    // Five pages of the main list at sign-on; every page after the first is held at the wire.
    let world = World::held(650);
    let (_, seq) = world.status();
    // Nothing waits for the whole list: what the first page landed is on the list already.
    world.until("the chats of the first page", || world.conversation("u100091").map(drop));
    // And it was told as it landed: the change feed carries the row before the list is whole.
    world.changed(seq, "the change of a first-page chat", |change| match change {
        Change::Conversation { conversation } if conversation.id == "u100091" => Some(()),
        _ => None,
    });
    assert!(world.conversation("u100050").is_some(), "{:#?}", world.calls());
    // The sign-on is still inside the chats: nothing past them was asked while a page waits.
    assert_eq!(world.called("GetContacts"), 0, "{:#?}", world.calls());
    // Let through, the rest of the chats and the archive are read and the sign-on finishes.
    world.steer("dialogs-go");
    world.connected();
    let known = |at: u32| world.conversation(&format!("u{}", 100_000 + at)).is_some();
    world.until("the newest 500", || known(491).then_some(()));
    assert_eq!((world.called("GetDialogs 0"), world.called("GetDialogs 1")), (5, 1), "{:#?}", world.calls());
}

#[test]
fn the_next_page_is_asked_while_the_current_one_is_written() {
    // The group's row is written only after its dialog was asked again (the store's read is
    // ahead of what the page says), and that ask is held at the wire: the writing of the page
    // sits on it the way any page's writing can on a slower network or a busy database.
    let world = World::stalled(650);
    world.until("the group's read to be told again", || (world.called("ReadHistory g8001 25") == 1).then_some(()));
    // While the page is still being written, the one after it is already at the wire.
    world.within(Duration::from_secs(3), "the page after to be asked", || {
        world.calls().iter().any(|call| call.starts_with("GetDialogs 0 u100091")).then_some(())
    });
    // When the ask goes through, the writing carries on and the sign-on finishes, one chat's
    // dialog asked only once.
    world.steer("peer-go");
    world.connected();
    assert_eq!(world.called("GetDialogs 0"), 5, "{:#?}", world.calls());
    assert_eq!(world.called("GetPeerDialogs g8001"), 1, "{:#?}", world.calls());
}

#[test]
fn the_prune_runs_after_the_chats_are_served() {
    // A message older than retention ever keeps here: Telegram's pages carry no conversation
    // for it, and a prune at boot would carry it off while the sign-on is still writing chats.
    let world = World::seed(Some(SESSION), "30", &[], |store| {
        let long_ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 200 * 86_400;
        let message = Message::plain("old-letter", "Long gone", "Wrote this before tinyclient.", long_ago, false);
        store.ingest(&key("u6543"), &message, false).unwrap();
    });
    world.connected();
    // The chats of the boot are on the list and the old message stands: retention did not run
    // inside the boot that served them.
    let prunable = |days: u32| match world.ask(Request::Prunable { days }) {
        Response::Prunable { messages, .. } => messages,
        other => panic!("{other:?}"),
    };
    assert_eq!(prunable(90), 1, "{:#?}", world.calls());
    // And retention still runs, at once when its setting asks it to.
    world.ask(Request::Set { scope: String::new(), key: "keep_days".to_owned(), value: Some("7".to_owned()) });
    world.until("the old message to be pruned", || (prunable(90) == 0).then_some(()));
}

#[test]
fn a_pin_or_a_folder_change_is_told_row_by_row_and_never_rereads_the_chats() {
    let world = World::signed_in();
    world.connected();
    let before = world.calls().len();
    let pin = |id: &str| world.conversation(id).unwrap().pin;

    // Pinned on another device: the group goes to the top of the pins. No request at all.
    world.steer("pin");
    world.until("the group to be pinned", || (pin("g8001") == Some(0)).then_some(()));
    assert_eq!(pin("u7002"), Some(1));
    assert_eq!(world.calls().len(), before, "{:#?}", &world.calls()[before..]);

    // Reordered without saying how: the pins alone are asked, once.
    world.steer("pins");
    world.until("the pins to be asked", || (world.called("GetPinnedDialogs 0") == 1).then_some(()));
    world.until("the new order", || (pin("g8001") == Some(0) && pin("u7002") == Some(1)).then_some(()));

    // A folder changed, with its rules in the update: refiled from what is held.
    world.steer("folder");
    world.until("Tobin in Family", || world.conversation("u7003").filter(|row| row.folders == ["3"]));
    let added: Vec<String> = world.calls()[before..].to_vec();
    assert_eq!(added, ["GetPinnedDialogs 0"], "one request for all three changes");
    assert_eq!(world.called("GetDialogs"), 2);
}

#[test]
fn a_photos_stripped_preview_is_on_disk_while_its_thumbnail_is_still_downloading() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_FILE_HOLD", "1")]);
    world.connected();
    let on_disk = |id: &str| match world.ask(Request::Fetch { account: account(), ids: vec![id.into()], size: None, retry: false, span: None }) {
        Response::Media { mut files } => files.pop().and_then(|file| file.path),
        other => panic!("{other:?}"),
    };
    // Posted while the chat is closed; opening it reads the page and wants what its rows show.
    world.steer("photo-preview");
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    let posted = world.arrived("u7002", "24");
    let attachment = &posted.attachments[0];
    let preview = attachment.preview.as_ref().expect("the photo carries its stripped thumbnail");
    assert_eq!((preview.id.as_str(), preview.kind, preview.width, preview.height), ("p6016i", MediaKind::Image, 40, 30));
    assert_eq!(attachment.thumb.as_ref().map(|thumb| thumb.id.as_str()), Some("p6016m"));

    // The row's size is asked of Telegram, which holds it; the preview is on disk regardless.
    world.until("the thumbnail asked for", || (world.called("GetFile photo 6016 m") > 0).then_some(()));
    let kept = world.until("the preview on disk", || on_disk("p6016i"));
    let jpeg = std::fs::read(&kept).unwrap();
    assert_eq!((&jpeg[..2], &jpeg[jpeg.len() - 2..]), (&[0xff, 0xd8][..], &[0xff, 0xd9][..]), "a whole JPEG");
    assert!(on_disk("p6016m").is_none(), "the thumbnail is still held at the wire");
    assert_eq!(world.called("GetFile photo 6016 i"), 0, "nothing is asked for the preview");
    // The stored message names where its preview is, for the next page read.
    let stored = world.message("u7002", "24").unwrap();
    assert_eq!(stored.attachments[0].preview.as_ref().and_then(|file| file.path.as_deref()), Some(kept.as_str()));

    // Let go, the thumbnail lands beside it; the preview stays for the next open.
    world.steer("file-go");
    let thumb = world.until("the thumbnail on disk", || on_disk("p6016m"));
    let served = include_bytes!("../src/adapters/telegram/fake_ridge_m.jpg");
    assert_eq!(std::fs::read(&thumb).unwrap(), served);
    assert_eq!(on_disk("p6016i").as_deref(), Some(kept.as_str()));
}

#[test]
fn a_thumbnail_gone_from_disk_is_never_marked_failed_and_telegram_keeps_it_again() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("history", || world.message("u7002", "12"));
    let asked = |id: &str| match world.ask(Request::Fetch { account: account(), ids: vec![id.into()], size: None, retry: false, span: None }) {
        Response::Media { mut files } => files.pop(),
        other => panic!("{other:?}"),
    };
    world.steer("photo");
    world.arrived("u7002", "14");
    let landed = world.until("the thumbnail on disk", || asked("p6014m").and_then(|file| file.path));
    // The cache evicted it, or something else deleted it: the row stays, the file is gone.
    std::fs::remove_file(&landed).unwrap();

    // The shell asks for it again. Only Telegram can fetch it: the cache must not try the
    // address it was kept under, which would mark it failed while Telegram brings it back.
    let again = asked("p6014m").expect("still known");
    assert_eq!((again.path.as_deref(), again.failed.as_deref()), (None, None));
    let deadline = Instant::now() + Duration::from_millis(1_500);
    while Instant::now() < deadline {
        let seen = asked("p6014m").expect("still known");
        assert!(seen.failed.is_none(), "marked failed: {seen:?}");
        sleep(Duration::from_millis(100));
    }
    assert!(!world.stderr().contains("does not serve files from that address"), "{}", world.stderr());

    // Telegram knows where it is, and the shell asked for it because it is on screen: it is
    // fetched again at once, ahead of what is not on screen, with no page read again for it.
    world.until("Telegram to keep it again", || asked("p6014m").and_then(|file| file.path));
    assert_eq!(world.called("GetFile photo 6014 m"), 4, "fetched once more, in its two pieces: {:#?}", world.calls());
}

#[test]
fn a_live_location_moves_on_the_card_and_the_move_is_not_a_new_message() {
    let world = World::signed_in();
    world.connected();
    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));
    world.until("history", || world.message("u7002", "12"));
    world.steer("live-place");
    let place = world.until("the live location", || world.message("u7002", "40").filter(|message| message.card.is_some()));
    let card = place.card.expect("a live location is a card");
    assert_eq!(card.title, "Live location");
    assert!(card.text.contains("geo:37.7,-122.4"), "{}", card.text);
    assert!(card.fields.iter().any(|(label, value)| label == "Status" && value == "Still sharing"), "{:?}", card.fields);
    world.until("the live location counts once", || world.conversation("u7002").filter(|row| row.unread == 2));
    world.steer("live-move");
    let moved = world.until("the live location moved", || {
        world.message("u7002", "40").filter(|message| message.card.as_ref().is_some_and(|card| card.text.contains("geo:37.8,-122.5")))
    });
    assert!(moved.card.unwrap().text.contains("geo:37.8,-122.5"));
    assert_eq!(world.conversation("u7002").unwrap().unread, 2);
}

#[test]
fn messages_arrive_change_and_go_and_what_they_hold_is_mapped() {
    let world = World::signed_in();
    world.connected();
    // The conversation is on screen, so what a new message shows is fetched as it arrives.
    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));
    // Opened first, so the page read on opening cannot take the new message in before the
    // update counts it.
    world.until("history", || world.message("u7002", "12"));

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
    // The row's count is written just after the message.
    world.until("the second unread", || world.conversation("u7002").filter(|row| row.unread == 2));

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
    assert_eq!((attachment.file.id.as_str(), attachment.file.kind, attachment.file.width, attachment.file.height), ("p6014y", MediaKind::Image, 1_280, 960));
    let thumb = attachment.thumb.as_ref().unwrap();
    assert_eq!((thumb.id.as_str(), thumb.width, thumb.height), ("p6014m", 320, 240));
    // Its stripped thumbnail names a size and no picture: there is no preview to draw.
    assert!(attachment.preview.is_none(), "{attachment:?}");
    let landed = world.until("the thumbnail on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["p6014m".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), FILE_BYTES);
    assert!(std::path::Path::new(&landed).parent().unwrap().ends_with("media/telegram/7001"), "{landed}");
    let calls = world.calls();
    assert!(calls.contains(&"ExportAuthorization 4".to_owned()) && calls.contains(&"ImportAuthorization".to_owned()), "{calls:#?}");
    assert_eq!(world.called("GetFile photo 6014 m"), 2, "{calls:#?}");
    // The stored message now names the file where it is.
    assert_eq!(world.message("u7002", "14").unwrap().attachments[0].thumb.as_ref().unwrap().path.as_deref(), Some(landed.as_str()));
    // The original is the largest size Telegram keeps. It is not fetched for a row; the viewer
    // asking for it fetches it, ahead of anything else waiting.
    assert_eq!(world.called("GetFile photo 6014 y"), 0, "{calls:#?}");
    let original = world.until("the original on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["p6014y".into()], size: Some(2_560), retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    assert_ne!(original, landed);
    assert_eq!(std::fs::metadata(&original).unwrap().len(), FILE_BYTES);
    assert_eq!(world.called("GetFile photo 6014 y"), 2);
    assert_eq!(world.called("GetFile photo 6014 x"), 0);

    world.steer("voice");
    let voice = world.arrived("u7002", "15");
    let note = &voice.attachments[0];
    assert_eq!((note.name.as_str(), note.file.kind, note.duration, note.mime.as_str()), ("Voice message", MediaKind::Audio, 7, "audio/ogg"));
    assert_eq!(note.waveform, vec![255, 0, 255, 0, 255, 0, 255, 0]);
    // Played only when asked: the note itself is not fetched for its row, and comes from
    // Telegram in one piece when the shell asks for it to play.
    assert_eq!(world.called("GetFile document 6100"), 0);
    let played = world.until("the voice message on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["d6100".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    assert_eq!(std::fs::metadata(&played).unwrap().len(), FILE_BYTES);
    assert_eq!(world.called("GetFile document 6100"), 1, "one piece of up to a megabyte");

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

    // What tinyclient cannot draw is named and linked, never dropped; a service message is a line.
    world.steer("poll");
    let poll = world.arrived("g8001", "21");
    assert_eq!(poll.text, "Poll: Saturday or Sunday?");
    assert_eq!(poll.kind, MessageKind::Unsupported { link: "tg://openmessage?chat_id=8001&message_id=21".into() });
    world.steer("service");
    let renamed = world.arrived("g8001", "22");
    assert_eq!((renamed.kind, renamed.text.as_str()), (MessageKind::System, "Tobin Reyes renamed this to \"Ridge crew\""));
}

#[test]
fn a_poll_shows_its_options_and_a_later_update_replaces_the_counts() {
    let world = World::signed_in();
    world.connected();
    world.steer("poll-full");
    let poll = world.arrived("g8001", "31");
    assert_eq!(poll.text, "Poll: Saturday or Sunday?");
    assert!(matches!(poll.kind, MessageKind::Unsupported { ref link } if link.contains("message_id=31")));
    let card = poll.card.expect("a poll is a card");
    assert_eq!(card.title, "Saturday or Sunday?");
    assert_eq!(card.footer, "Open in Telegram");
    assert_eq!(card.fields, vec![("3 votes".to_owned(), "Saturday".to_owned()), ("1 vote".to_owned(), "Sunday".to_owned())]);
    world.steer("poll-update");
    let updated = world.until("the new counts", || {
        let message = world.message("g8001", "31")?;
        let card = message.card.as_ref()?;
        (card.fields.first().is_some_and(|(label, _)| label == "5 votes")).then_some(message)
    });
    let card = updated.card.expect("the card stays");
    assert_eq!(card.fields, vec![("5 votes".to_owned(), "Saturday".to_owned()), ("2 votes".to_owned(), "Sunday".to_owned())]);
    assert_eq!(card.text, "7 votes");
    // Counts alone name options by their bytes, which the stored card does not keep.
    world.steer("poll-counts");
    sleep(Duration::from_millis(200));
    let card = world.message("g8001", "31").and_then(|message| message.card).expect("the card stays");
    assert_eq!(card.fields[0].0, "5 votes");
}

#[test]
fn a_message_whose_own_time_has_passed_is_nowhere_and_one_still_alive_leaves_the_store_offline_too() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });

    // One message long past its own time, one with a moment still to live.
    world.steer("ttl");
    let alive = world.arrived("u7002", "18");
    assert_eq!(alive.expires_at, Some(alive.sent + 10), "the timer Telegram named is kept with the message");
    // The spent one is in no read, though it was in the same conversation.
    assert!(world.message("u7002", "17").is_none());
    assert!(world.messages("u7002").into_iter().all(|message| message.id != "17"));
    // And the live one stands.
    assert!(world.message("u7002", "18").is_some());

    // At the live one's moment the store drops what ran out, online or not, and says so to
    // the shell.
    let asked = world.called("GetHistory u7002");
    let mut since = world.status().1;
    let (mut messages_changed, mut preview_changed) = (false, false);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !messages_changed || !preview_changed {
        assert!(Instant::now() < deadline, "expiry did not announce messages and sidebar preview: {messages_changed}, {preview_changed}");
        let Response::Changes { seq, changes, .. } = world.ask(Request::Wait { since }) else {
            panic!("no changes");
        };
        since = seq;
        // Consume the initial arrival's changes before looking for expiry's changes.
        if world.message("u7002", "18").is_some() {
            continue;
        }
        messages_changed |= changes.iter().any(|change| matches!(change,
            Change::Messages { conversation } if conversation.id == "u7002"));
        preview_changed |= changes.iter().any(|change| matches!(change,
            Change::Conversation { conversation } if conversation.id == "u7002"));
    }
    assert!(world.message("u7002", "18").is_none());
    assert_ne!(world.conversation("u7002").unwrap().preview, alive.text);
    assert!(world.messages("u7002").into_iter().all(|message| message.id != "17"));
    assert_eq!(world.called("GetHistory u7002"), asked, "nothing was asked of Telegram about it");

    // Telegram says both are gone after their time. The store let them go already, so its word
    // changes nothing here: no change at all.
    world.steer("ttl-gone");
    let quiet = world.status().1;
    sleep(Duration::from_secs(2));
    assert_eq!(world.status().1, quiet, "{:#?}", world.calls());
}

#[test]
fn an_unopened_topics_expiring_forum_preview_leaves_the_sidebar_at_its_own_time() {
    expiring_forum_preview(false);
}

#[test]
fn an_opened_topics_expiring_forum_preview_leaves_the_sidebar_with_its_message() {
    expiring_forum_preview(true);
}

fn expiring_forum_preview(open_topic: bool) {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_FORUM_TTL", "1")]);
    world.connected();
    world.until("the forum's temporary preview", || world.conversation("c9002")
        .filter(|row| row.preview == "Who has the torque wrench?"));
    if open_topic {
        world.watch(Watching { conversation: Some(key("c9002")), ..Watching::default() });
        world.messages("c9002");
        world.until("the topic row", || world.conversation("c9002_31"));
        world.watch(Watching { conversation: Some(key("c9002_31")), ..Watching::default() });
        world.arrived("c9002_31", "41");
    } else {
        assert_eq!(world.called("GetForumTopics"), 0, "the topic has never been opened");
        assert_eq!(world.called("GetReplies c9002_31"), 0);
    }
    let since = world.status().1;
    world.changed(since, "the expired forum preview to leave the sidebar", |change| match change {
        Change::Conversation { conversation } if conversation.id == "c9002"
            && world.conversation("c9002").is_some_and(|row| row.preview.is_empty()) => Some(()),
        _ => None,
    });
    let row = world.conversation("c9002").unwrap();
    assert_eq!((row.preview.as_str(), row.preview_from.as_str(), row.preview_mine, row.updated), ("", "", false, 0));
    let store = chatd::store::Store::open(&world.dir.join("chat.sqlite")).unwrap();
    assert!(store.message(&key("c9002"), "41").unwrap().is_none(), "no duplicate parent message");
    assert!(store.message(&key("c9002_31"), "41").unwrap().is_none());
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
    // Read in a topic is told for the topic. Before a reply: a reply moves the marker ahead
    // of it, and a read of an older message is already covered by that.
    world.ask(Request::MarkRead { conversation: key("c9002_31"), message_id: "42".into() });
    world.until("the topic to be read", || world.calls().contains(&"ReadDiscussion c9002_31 42".to_owned()).then_some(()));
    world.send("c9002_31", "9001", "On my way", None);
    world.until("the post to be sent", || world.calls().contains(&"SendMessage c9002_31 On my way".to_owned()).then_some(()));
    world.until("it to be in the thread", || world.messages("c9002_31").into_iter().find(|message| message.text == "On my way" && message.state == SendState::Sent));

    // A topic row on screen with no line yet: its newest message is asked for, one only.
    world.rows_on_screen(&["c9002_1"]);
    world.until("the topic's preview to be asked", || (world.called("GetReplies c9002_1") == 1).then_some(()));
    // A row the dialog list already gave a line asks nothing.
    world.rows_on_screen(&["u7002"]);
    sleep(Duration::from_millis(300));
    assert_eq!(world.called("GetHistory u7002"), 0);

    // Read in a topic is told for the topic.
    world.ask(Request::MarkRead { conversation: key("c9002_31"), message_id: "42".into() });
    world.until("the topic to be read", || world.calls().contains(&"ReadDiscussion c9002_31 42".to_owned()).then_some(()));
    // The topic's own color, the tile the list draws its icon with, and its picture: the
    // icon's thumbnail lands on disk and the row shows it.
    let topic = world.conversation("c9002_31").unwrap();
    assert_eq!(topic.topic_icon, Some(3), "the topic's own icon color");
    world.until("the topic's picture to land", || world.conversation("c9002_31").filter(|row| row.avatar.as_ref().and_then(|file| file.path.as_ref()).is_some()).map(|_| ()));
    let path = world.conversation("c9002_31").unwrap().avatar.as_ref().and_then(|file| file.path.as_ref()).unwrap().to_owned();
    // The picture arrives as Telegram serves it, a gzip member: what is kept is the file
    // inside, which the shell can read as its picture or animation.
    let kept = std::fs::read(&path).expect("the topic's picture is on disk");
    assert!(kept.starts_with(b"{"), "the gzip member is unpacked to what is inside: {:02x?}", &kept[..kept.len().min(8)]);

    // General's own icon is an animated emoji — no still thumbnail named in its document.
    // It is asked whole: its file lands, and what lands is the Lottie JSON it is drawn as,
    // the same still the official list shows before the frames play.
    world.until("General's animated icon to land", || world.conversation("c9002_1").filter(|row| row.avatar.as_ref().and_then(|file| file.path.as_ref()).is_some()).map(|_| ()));
    let moved = world.conversation("c9002_1").unwrap().avatar.as_ref().and_then(|file| file.path.as_ref()).and_then(|path| std::fs::read(path).ok()).expect("General's animated icon is on disk");
    assert!(moved.starts_with(b"{"), "the animated emoji's file is what is drawn: {:02x?}", &moved[..moved.len().min(8)]);
}

#[test]
fn a_custom_emoji_reactions_document_is_asked_and_its_file_lands_for_the_chip() {
    let world = World::signed_in();
    world.connected();
    // The conversation is on screen, so the reaction's picture is asked for as it arrives.
    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));
    world.until("history", || world.message("u7002", "12"));
    let before = world.called("GetCustomEmojiDocuments");
    // A reaction that is a custom emoji arrives: its document is named in one batch, and the
    // reaction carries the media the chip draws (`e<document id>`), stills keep their text.
    world.steer("custom-react");
    let reacted = world.until("the custom reaction", || {
        world.message("u7002", "12").filter(|message| message.reactions.iter().any(|have| have.file.is_some()))
    });
    let seen: Vec<(&str, &str)> = reacted.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.file.as_ref().map_or("", |file| file.id.as_str()))).collect();
    assert_eq!(seen, vec![("👍", ""), (":emoji:", "e5002")]);
    world.until("the emoji document asked for", || (world.called("GetCustomEmojiDocuments") > before).then_some(()));
    // The file is fetched whole (the animated document) and kept under the name the
    // reaction carries, unpacked from the gzip Telegram serves it in.
    let landed = world.until("the reaction picture on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    let kept = std::fs::read(&landed).expect("the reaction picture is on disk");
    assert!(kept.starts_with(b"{"), "what a chip draws is the file inside: {:02x?}", &kept[..kept.len().min(8)]);
}

#[test]
fn a_premium_emoji_in_message_text_is_asked_and_its_file_lands_for_the_row() {
    let world = World::signed_in();
    world.connected();
    // The boot reads each dialog's first page at its own pace, and a page read asks the
    // documents its spans name — a global count taken before this test opens the chat races
    // that paced read. The chat is opened first, and the count is taken settled.
    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));
    world.until("the page's own message", || world.message("u7002", "12"));
    let before = world.called("GetCustomEmojiDocuments");
    // A premium emoji arrives in the open chat: one batch asks the document its span names
    // (`e<document id>`), the file lands whole under that name, and the text keeps the alt
    // glyph until it is there.
    world.steer("emoji-text");
    world.until("the emoji document asked for", || (world.called("GetCustomEmojiDocuments") > before).then_some(()));
    let read = world.until("the page with a premium emoji", || world.message("u7002", "22"));
    let named = |message: &Message| {
        let span = message.spans.iter().find(|span| matches!(span.style, Style::Emoji { .. })).expect("the run names an emoji");
        let Style::Emoji { file } = &span.style else { unreachable!() };
        (message.text[span.start as usize..span.end as usize].to_owned(), file.id.clone())
    };
    assert_eq!(named(&read), ("🦀".to_owned(), "e5002".to_owned()));
    world.until("the emoji document asked for", || (world.called("GetCustomEmojiDocuments 5002") > 0).then_some(()));
    let landed = world.until("the message emoji on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    let kept = std::fs::read(&landed).expect("the message emoji is on disk");
    assert!(kept.starts_with(b"{"), "what the row draws is the file inside: {:02x?}", &kept[..kept.len().min(8)]);

    // One that arrives while the conversation is on screen is fetched as it lands.
    world.steer("emoji-text-live");
    let live = world.until("the premium emoji to arrive", || world.message("u7002", "23"));
    assert_eq!(named(&live), ("🔥".to_owned(), "e5001".to_owned()));
    world.until("the still emoji asked for", || (world.called("GetCustomEmojiDocuments 5001") > 0).then_some(()));
    world.until("the still emoji on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5001".into()], size: None, retry: false, span: None }) {
            Response::Media { files } => files.first().and_then(|file| file.path.clone()),
            other => panic!("{other:?}"),
        }
    });
}

#[test]
fn a_channel_posts_comments_count_rides_the_post() {
    let world = World::signed_in();
    world.connected();
    // The channel is opened: its post is stored, with nothing said of comments yet. The edit is
    // sent only after the open's last ask, the sponsored message, so it lands on a settled channel.
    world.until("the channel's page", || world.message("c9001", "5"));
    world.until("the channel to finish opening", || (world.called("GetSponsoredMessages c9001") >= 1).then_some(()));
    world.steer("comments");
    // Telegram says the section on the post itself, as an edit: the count is its own, and the
    // id it names is the one the footer opens the comments under. The post was already stored, so
    // wait for the edit to land on it rather than reading it the moment it exists.
    let post = world.until("the post's comment count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    let section = post.thread.as_ref().expect("the post carries its comment count");
    assert_eq!((section.replies, section.id.as_str(), section.last), (2, "c9001_5", 0));
    let thread = world.conversation("c9001_5").unwrap();
    assert_eq!(
        (thread.kind, thread.title.as_str(), thread.parent.as_deref(), thread.root.as_deref(), thread.read_only),
        (ConversationKind::Thread, "Comments", Some("c9001"), Some("5"), true),
    );
}

#[test]
fn a_channel_posts_views_ride_the_post_from_one_ask_for_the_page() {
    let world = World::signed_in();
    world.connected();
    // Opening the channel reads the page once, and the views come with it: the count rides the
    // post its footer draws, not the channel's row.
    let post = world.until("the post's views", || world.message("c9001", "5").filter(|post| post.views.is_some()));
    assert_eq!(post.views, Some(12_400), "{:#?}", world.calls());
    assert!(world.called("GetMessagesViews c9001 5") >= 1, "{:#?}", world.calls());
    // A direct chat has no views, and none is asked for.
    world.until("history", || world.message("u7002", "12"));
    assert_eq!(world.called("GetMessagesViews u7002"), 0, "{:#?}", world.calls());
}

#[test]
fn a_channel_post_shows_its_signature_and_shares_and_is_counted_as_viewed_once_read() {
    let world = World::signed_in();
    world.connected();
    // The page's one ask brings the shares with the views, and counts nothing.
    let post = world.until("the post's counts", || world.message("c9001", "5").filter(|post| post.views.is_some()));
    assert_eq!((post.views, post.shares, post.signature.as_str()), (Some(12_400), Some(31), ""));
    assert_eq!(world.called("GetMessagesViews c9001 5 increment"), 0);
    world.until("the channel to finish opening", || (world.called("GetSponsoredMessages c9001") >= 1).then_some(()));

    // The author signs the post: the signature rides the message, and the counts stay.
    world.steer("signed");
    let signed = world.until("the signature", || world.message("c9001", "5").filter(|post| !post.signature.is_empty()));
    assert_eq!((signed.signature.as_str(), signed.views, signed.shares), ("Mira K.", Some(12_400), Some(31)));

    // Read on screen: Telegram is told to count the view, once, and its new count is written.
    world.ask(Request::MarkRead { conversation: key("c9001"), message_id: "5".into() });
    world.until("the view to be counted", || world.message("c9001", "5").filter(|post| post.views == Some(12_401)));
    // Read again, then a private chat read after it: the adapter takes its commands in order,
    // so once the chat's read is told the second read of the post has been handled too.
    world.ask(Request::MarkRead { conversation: key("c9001"), message_id: "5".into() });
    world.arrived("u7002", "12");
    world.ask(Request::MarkRead { conversation: key("u7002"), message_id: "12".into() });
    world.until("the chat's read to be told", || (world.called("ReadHistory u7002") >= 1).then_some(()));
    assert_eq!(world.called("GetMessagesViews c9001 5 increment"), 1, "{:#?}", world.calls());
    // A private chat's read counts no view.
    assert_eq!(world.called("GetMessagesViews u7002"), 0);

    // Telegram tells the post's new counts while the channel is open.
    world.steer("post-counts");
    let live = world.until("the live counts", || world.message("c9001", "5").filter(|post| post.shares == Some(40)));
    assert_eq!((live.views, live.signature.as_str()), (Some(13_000), "Mira K."));
}

#[test]
fn a_posts_unread_comments_ride_its_footer_until_they_are_read() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments 3");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    // The discussion says three comments are unread: opening it puts them on the post, whose
    // footer shows them, and a page of the channel read later does not stamp them away.
    world.messages("c9001_5");
    let post = world.until("the post's unread comments", || {
        world.message("c9001", "5").filter(|post| post.thread.as_ref().is_some_and(|section| section.unread == 3))
    });
    let section = post.thread.as_ref().unwrap();
    assert_eq!(section.replies, 2, "{section:?}");
}

#[test]
fn asking_a_channel_posts_comments_reads_the_discussion_group_under_the_post() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));

    // The thread opens on its own id: Telegram is asked once for the replies to the post, and
    // the comments are its answer, the discussion group's people and messages.
    world.until("the comments", || {
        let page = world.messages("c9001_5");
        (page.len() == 2).then_some(page)
    });
    // The comments as the shell reads a page: oldest first.
    let comments: Vec<(String, String, String)> =
        world.messages("c9001_5").into_iter().map(|comment| (comment.id, comment.sender.name, comment.text)).collect();
    let said: Vec<(&str, &str, &str)> = comments.iter().map(|comment| (comment.0.as_str(), comment.1.as_str(), comment.2.as_str())).collect();
    assert_eq!(said, [("61", "Tobin Reyes", "On my way up front?"), ("62", "Odalys Brandt", "Yes, at the hut now.")]);
    assert_eq!(world.called("GetReplies c9001_5"), 1, "{:#?}", world.calls());
    assert_eq!(world.called("GetHistory c9001_5"), 0);

    // The newest comment's time rides the post's footer, as "last reply", once the page was
    // read: the section itself names no date. Comment 62 went 40 seconds before the fake's clock start.
    let fresh = world.until("the footer's last reply", || {
        world.message("c9001", "5").filter(|post| post.thread.as_ref().is_some_and(|section| section.last == t0() - 40))
    });
    let section = fresh.thread.as_ref().unwrap();
    assert_eq!(section.replies, 2, "{section:?}");
}

#[test]
fn opening_a_comments_thread_asks_telegram_once_where_its_comments_live_and_lets_it_post() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));

    // Opened: Telegram is asked for the discussion group behind the post and the echo of it
    // to answer, once, and the row stops saying read-only once it did as much. Reading the
    // comments themselves goes on under the post's own id, as before.
    world.messages("c9001_5");
    world.until("the replies to be read", || {
        (world.called("GetDiscussionMessage c9001_5") == 1 && world.called("GetReplies c9001_5") == 1).then_some(())
    });
    let open = world.until("the row to let a comment in", || {
        world.conversation("c9001_5").filter(|row| !row.read_only)
    });
    assert_eq!((open.kind, open.title.as_str()), (ConversationKind::Thread, "Comments"));

    // Opened again: the answer stands for the connection and nothing is asked again.
    world.messages("c9001_5");
    sleep(Duration::from_millis(400));
    assert_eq!(world.called("GetDiscussionMessage c9001_5"), 1, "{:#?}", world.calls());
}

#[test]
fn a_comment_posted_in_the_thread_is_told_as_a_reply_to_the_posts_echo_in_the_discussion_group() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    world.until("the discussion to be resolved", || world.conversation("c9001_5").filter(|row| !row.read_only));

    // Telegram lists the newer comment (62) before the forwarded post (60). The send must
    // answer the post's echo, which anchors the discussion in the group.
    world.send("c9001_5", "9002", "Sunset push", None);
    world.until("the comment to be told", || {
        world.calls().contains(&"SendMessage c9003_60 Sunset push".to_owned()).then_some(())
    });
    // The echo was learned with the thread's open: the send asked Telegram nothing of it again.
    sleep(Duration::from_millis(400));
    assert_eq!(world.called("GetDiscussionMessage c9001_5"), 1, "{:#?}", world.calls());
}

#[test]
fn a_sent_comment_lands_in_the_thread_and_the_posts_count_rises_under_its_own_id() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    world.until("the discussion to be asked", || (world.called("GetDiscussionMessage c9001_5") == 1).then_some(()));
    // The resolve answered: the flip of the row's own read-only word follows it onto the
    // store. A send that asked before it would be refused, and rightly so - the test waits
    // for the row the way a window does.
    world.until("the thread to be postable", || world.conversation("c9001_5").filter(|row| !row.read_only).map(|_| ()));

    // The echo Telegram sends lands as the thread's comment, not as a message of the
    // discussion group's own list; the quote line a comment answers in the post's echo is
    // the thread's own anchor and names nobody.
    world.send("c9001_5", "8001", "Sunset push", None);
    let sent = world.until("the comment in the thread", || {
        world.messages("c9001_5").into_iter().find(|comment| comment.text == "Sunset push" && comment.state == SendState::Sent)
    });
    assert_eq!((sent.outgoing, sent.reply_to.is_none()), (true, true), "{sent:?}");

    // Telegram raised the count on the post itself, without moving the id the footer
    // opens the thread under.
    let post = world.until("the count to rise", || {
        world.message("c9001", "5").filter(|post| post.thread.as_ref().is_some_and(|section| section.replies == 3))
    });
    let section = post.thread.as_ref().unwrap();
    assert_eq!(section.id.as_str(), "c9001_5");
    assert_eq!(world.conversation("c9001_5").unwrap().root.as_deref(), Some("5"));
}

#[test]
fn a_discussion_group_stops_counting_unread_after_leaving_on_another_device() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comment section", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    world.until("the comments to be writable", || world.conversation("c9001_5").filter(|row| !row.read_only));
    world.send("c9001_5", "8001", "My comment on the first post", None);
    world.until("the comment to be sent", || world.messages("c9001_5").into_iter().find(|m| m.state == SendState::Sent && m.outgoing));

    world.steer("discussion-membership joined");
    world.steer("discussion-comment 80 81");
    world.until("unread activity in the joined group", || world.conversation("c9003").filter(|row| row.unread == 1));

    world.steer("discussion-membership left");
    world.steer("discussion-comment 80 84");
    world.steer("discussion-reply");
    let reply = world.arrived("u1271266957", "83");
    assert_eq!(reply.sender.name, "Tobin Reyes");
    assert!(reply.mentions_me);
    assert_eq!(reply.forwarded.unwrap().link, "https://t.me/c/9003/82");
    assert_eq!(world.conversation("u1271266957").map(|row| (row.unread, row.read_only)), Some((1, true)));
    assert!(world.conversation("c9003").is_none(), "the group row came back");
    assert!(world.message("u7001", "83").is_none(), "the notification was copied into Saved Messages");
}

#[test]
fn a_nested_comment_lands_on_the_open_post_without_an_unread_badge_for_a_nonmember() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comment section", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    world.until("the discussion to be resolved", || world.conversation("c9001_5").filter(|row| !row.read_only));
    world.watch(Watching { conversation: Some(key("c9001_5")), ..Watching::default() });
    world.steer("discussion-comment 60 85");
    let reply = world.arrived("c9001_5", "85");
    assert_eq!(reply.text, "A new discussion comment");
    assert_eq!(world.conversation("c9001_5").unwrap().unread, 0);
    assert!(world.conversation("c9003").is_none());
}

#[test]
fn a_cached_unread_group_left_while_offline_does_not_keep_its_badge_on_reconnect() {
    let world = World::seed(Some(SESSION), "30", &[], |store| {
        let group = key("c9003");
        let row = chatd::store::ConversationRow::new("c9003", ConversationKind::Group, "Field Notes chat");
        store.upsert_conversation(&group.account(), &row).unwrap();
        store.set_counts(&group, 3, 0, None, t0()).unwrap();
        store.set_name(&group.account(), "peer:c9003", "9003009\tField Notes chat").unwrap();
    });
    world.connected();
    assert_eq!(world.called("GetChannels [9003]"), 1);
    assert!(world.conversation("c9003").is_none());
    assert!(world.conversation("c9001").is_some());
}

#[test]
fn a_partial_update_for_a_joined_group_outside_the_dialogs_uses_its_saved_access_hash() {
    let world = World::seed(Some(SESSION), "30", &[], |store| {
        store.set_name(&account(), "peer:c9006", "9006009\tWeather desk").unwrap();
    });
    world.connected();
    assert!(world.conversation("c9006").is_none());

    world.steer("off-page-group");
    assert_eq!(world.arrived("c9006", "101").text, "The wind has picked up.");
    assert_eq!(world.conversation("c9006").unwrap().unread, 1);
    assert_eq!(world.called("GetPeerDialogs c9006"), 1);
    assert_eq!(world.called("ResolveUsername weatherdesk"), 0);
}

#[test]
fn a_partial_update_for_a_public_group_can_resolve_its_username_without_a_saved_hash() {
    let world = World::signed_in();
    world.connected();
    assert!(world.conversation("c9006").is_none());

    world.steer("off-page-group");
    assert_eq!(world.arrived("c9006", "101").text, "The wind has picked up.");
    assert_eq!(world.conversation("c9006").unwrap().unread, 1);
    assert_eq!(world.called("ResolveUsername weatherdesk"), 1);
}

#[test]
fn recovering_a_muted_group_retries_a_flood_wait_and_counts_held_messages_once() {
    let world = World::signed_in();
    world.connected();
    world.steer("off-page-group 101 muted flood");
    world.until("Telegram to defer the dialog lookup", || (world.called("GetPeerDialogs c9006 flood") == 1).then_some(()));
    assert!(world.conversation("c9006").is_none());
    world.steer("off-page-group 102 muted");
    // No new update is needed to trigger the retry after the flood wait.
    world.arrived("c9006", "101");
    world.arrived("c9006", "102");
    let row = world.conversation("c9006").unwrap();
    assert!(row.muted && row.archived);
    assert_eq!((row.unread, row.mentions), (2, 0));
    assert_eq!(row.badge(), protocol::Badge::None);
    assert_eq!(world.called("GetPeerDialogs c9006"), 2);
}

#[test]
fn a_private_group_without_a_hash_is_recovered_from_the_remaining_dialogs() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_MANY", "500"), ("TINYCLIENT_TELEGRAM_PRIVATE_OFF_PAGE", "1")]);
    world.connected();
    assert!(world.conversation("c9006").is_none());
    let before = world.called("GetDialogs 0");
    world.steer("off-page-group");
    world.arrived("c9006", "101");
    assert_eq!(world.conversation("c9006").unwrap().unread, 1);
    assert_eq!(world.called("GetDialogs 0"), before + 1);
    assert_eq!(world.called("ResolveUsername weatherdesk"), 0);
}

#[test]
fn an_update_queued_during_dialog_recovery_does_not_double_count_the_snapshot() {
    let world = World::signed_in();
    world.connected();
    world.steer("off-page-group 101 burst");
    world.arrived("c9006", "101");
    world.arrived("c9006", "102");
    assert_eq!(world.conversation("c9006").unwrap().unread, 2);
    world.steer("off-page-group 103");
    world.arrived("c9006", "103");
    assert_eq!(world.conversation("c9006").unwrap().unread, 3);
}

#[test]
fn a_private_broadcast_and_an_invalid_peer_do_not_block_reconnect_cleanup_of_other_chats() {
    let world = World::seed(Some(SESSION), "30", &[], |store| {
        for (id, kind, title, note) in [
            ("c9003", ConversationKind::Group, "Field Notes chat", "9003009\tField Notes chat"),
            ("c9004", ConversationKind::Channel, "Old broadcasts", "9004009\tOld broadcasts"),
            ("c9005", ConversationKind::Channel, "Needs verification", "9005009\tNeeds verification"),
        ] {
            let row = chatd::store::ConversationRow::new(id, kind, title);
            store.upsert_conversation(&account(), &row).unwrap();
            store.set_counts(&key(id), 3, 1, None, t0()).unwrap();
            store.set_name(&account(), &format!("peer:{id}"), note).unwrap();
        }
    });
    world.connected();
    assert_eq!(world.called("GetChannels [9003, 9004, 9005]"), 1);
    assert!(world.conversation("c9003").is_none());
    assert!(world.conversation("c9004").is_none());
    let unresolved = world.conversation("c9005").unwrap();
    assert_eq!((unresolved.unread, unresolved.mentions), (3, 1));
    assert!(world.conversation("c9001").is_some());
}

#[test]
fn unread_cleanup_resumes_past_one_hundred_peers_after_reconnect_and_retries_a_failed_range() {
    let world = World::seed(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_MEMBERSHIP_FAULT", "1")], |store| {
        for id in (20_000..20_100).chain([9_004]) {
            let row = chatd::store::ConversationRow::new(format!("c{id}"), ConversationKind::Channel, "Cached channel");
            store.upsert_conversation(&account(), &row).unwrap();
            store.set_counts(&key(&row.id), 3, 1, None, t0()).unwrap();
            store.set_name(&account(), &format!("peer:{}", row.id), &format!("{}\tCached channel", id * 1_000 + 9)).unwrap();
        }
    });
    world.connected();
    assert_eq!(world.called("GetChannels [20000,"), 1);
    assert!(world.conversation("c9004").is_some());
    world.steer("drop");
    world.until("the next connection to retry the older channel after its flood wait", || {
        (world.called("GetChannels [9004]") >= 2 && world.conversation("c9004").is_none()).then_some(())
    });
    assert_eq!(world.called("GetChannels [20000,"), 1);
    assert_eq!(world.conversation("c20000").unwrap().unread, 3);
}

#[test]
fn a_comments_reactions_document_is_asked_and_its_file_lands_for_the_chip_like_a_normal_messages_does() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    // A comment carries a reaction that is a custom emoji, before the thread is opened.
    world.steer("comment-react");
    let before = world.called("GetCustomEmojiDocuments");

    // Opening the thread reads the comments as one page: the reaction's document is asked
    // for the way a normal message's reaction does, and the stored comment names the media
    // the chip draws (`e<document id>`), the plain one keeping its glyph.
    world.messages("c9001_5");
    let reacted = world.until("the comment's reactions", || {
        world.message("c9001_5", "62").filter(|comment| !comment.reactions.is_empty())
    });
    let seen: Vec<(&str, &str)> = reacted.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.file.as_ref().map_or("", |file| file.id.as_str()))).collect();
    assert_eq!(seen, vec![("👍", ""), (":emoji:", "e5002")]);
    assert!(world.called("GetCustomEmojiDocuments 5002") > before, "{:#?}", world.calls());

    // The file lands whole under the name the reaction carries, as any reaction's does.
    let landed = world.until("the comment's reaction picture on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    let kept = std::fs::read(&landed).expect("the comment's reaction picture is on disk");
    assert!(kept.starts_with(b"{"), "what a chip draws is the file inside: {:02x?}", &kept[..kept.len().min(8)]);
}

#[test]
fn a_reaction_told_for_a_comment_lands_on_the_thread_while_it_is_open() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    // The thread is open and on screen: the open learned where its comments go, as the
    // window's own open does before a reaction could arrive there.
    world.until("the discussion to be asked", || (world.called("GetDiscussionMessage c9001_5") == 1).then_some(()));
    world.watch(Watching { conversation: Some(key("c9001_5")), ..Watching::default() });

    // Telegram says the reaction on the discussion group's stream, naming the echo the
    // comment answers: it lands on the stored comment in the thread, and its document is
    // asked for the chip, as a normal message's reaction is.
    world.steer("comment-react-live");
    let reacted = world.until("the reaction on the comment", || {
        world.message("c9001_5", "62").filter(|comment| !comment.reactions.is_empty())
    });
    let seen: Vec<(&str, u32)> = reacted.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.count)).collect();
    assert_eq!(seen, vec![("👍", 2), (":emoji:", 1)]);
    assert!(world.called("GetCustomEmojiDocuments 5002") > 0, "{:#?}", world.calls());
}

#[test]
fn a_draft_in_the_comments_thread_is_kept_on_the_thread_not_the_channel() {
    let world = World::signed_in();
    world.connected();
    world.steer("comments");
    world.until("the post's comments count", || world.message("c9001", "5").filter(|post| post.thread.is_some()));
    world.messages("c9001_5");
    world.until("the discussion to be asked", || (world.called("GetDiscussionMessage c9001_5") == 1).then_some(()));

    // A draft typed on the thread is kept where Telegram's own apps keep one: in the
    // discussion group, under the echo of the post — not against the channel post itself.
    world.ask(Request::SetDraft { conversation: key("c9001_5"), text: "Bringing the rope.".into() });
    world.until("the thread's draft to be told", || (world.called("SaveDraft c9003_60 Bringing the rope.") == 1).then_some(()));
    assert!(!world.calls().iter().any(|call| call.starts_with("SaveDraft c9001 ")), "{:#?}", world.calls());

    // And a draft the phone typed in the thread comes back on the thread's row.
    world.steer("thread-draft");
    world.until("the draft typed in the thread", || {
        world.conversation("c9001_5").filter(|row| row.draft == "Ropes at the hut, then.").map(|_| ())
    });
}

#[test]
fn a_chat_pinned_by_a_folder_beyond_the_pages_gets_its_row() {
    // The Work folder pins a chat that no dialog page the sign-on read carries: only the
    // folder rule speaks of it, and what nothing stored says of is asked for alone.
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_LOST_PIN", "1")]);
    world.connected();
    world.until("the pinned chat to be asked for", || (world.called("GetPeerDialogs g8001") >= 1).then_some(()));
    assert!(world.conversation("g8001").is_some(), "the folder's pinned chat gets its row");
}

#[test]
fn a_draft_typed_on_the_phone_shows_as_its_conversations_draft() {
    let world = World::signed_in();
    world.connected();
    // Odalys had a draft typed elsewhere before this sign-in: her dialog carried it, and the
    // row says so.
    assert_eq!(world.conversation("u7002").unwrap().draft, "Finished packing. Meet at the ridge?");
    // A draft the phone types while we are online follows the row just the same.
    world.steer("draft");
    world.until("the draft typed on the phone", || {
        world.conversation("u7002").filter(|row| row.draft == "Riding up with Tobin.").map(|_| ())
    });
    // And one the phone cleared empties the row again.
    world.steer("draft-none");
    world.until("the draft the phone cleared", || {
        world.conversation("u7002").filter(|row| row.draft.is_empty()).map(|_| ())
    });
}

#[test]
fn a_draft_kept_here_is_told_to_telegram_and_only_once() {
    let world = World::signed_in();
    world.connected();
    // What the user settled on here is asked of Telegram with the same words, and a cleared
    // draft is asked of it empty.
    world.ask(Request::SetDraft { conversation: key("u7002"), text: "Bringing the map, then.".into() });
    world.until("the draft to be told", || (world.called("SaveDraft u7002 Bringing the map, then.") == 1).then_some(()));
    world.ask(Request::SetDraft { conversation: key("u7002"), text: String::new() });
    world.until("the clear to be told", || (world.called("SaveDraft u7002") == 2).then_some(()));
    // The same for a group, where the draft is the chat's and not the person's. What the store
    // kept says so on the row, until the echo of the save says no more than that.
    world.ask(Request::SetDraft { conversation: key("g8001"), text: "Snacks are on me".into() });
    world.until("the group's draft to be told", || (world.called("SaveDraft g8001 Snacks are on me") == 1).then_some(()));
    assert_eq!(world.conversation("g8001").unwrap().draft, "Snacks are on me");
    // Nothing of the echo litters the log: one save per settled draft.
    sleep(Duration::from_millis(400));
    assert_eq!(world.called("SaveDraft g8001"), 1, "{:#?}", world.calls());
}

#[test]
fn a_forum_opened_while_the_link_was_down_comes_back_at_the_next_connect() {
    let world = World::signed_in();
    world.connected();
    // Telegram drops. The user opens the forum while it is down, nothing leaves chatd, the
    // store keeps its empty answer, and the ask stands held for the next connect, which
    // hears of it and fills the list.
    world.steer("drop");
    let _ = world.messages("c9002");
    world.until("the forum's topics to be asked again", || (world.called("GetForumTopics c9002") >= 1).then_some(()));
    world.until("the topics on the list", || world.conversation("c9002_1").map(|_| ()));
}

#[test]
fn a_read_lost_on_the_way_is_told_again_and_the_badge_stays() {
    let world = World::signed_in();
    world.connected();
    // Telegram never heard that the group was read up to 20, the way it goes when a tell went
    // out while the link was down for it: its dialog counts unread again, up to what it had.
    world.steer("lost-read");
    // A default notify change defers the chats to when they are asked for, which is now.
    world.steer("notify-changed");
    world.ask(Request::Home { network: Network::Telegram, account: None, folder: None, shelf: Shelf::Main, offset: 0, limit: 200 });
    // The read goes again, and what that tell left is counted, so the badge is not pushed back
    // to the unread the network forgot.
    world.until("the read to be told again", || (world.called("ReadHistory g8001 20") == 1).then_some(()));
    world.until("the counts to be asked again", || (world.called("GetPeerDialogs g8001") == 1).then_some(()));
    let read = world.conversation("g8001").unwrap();
    assert_eq!((read.unread, read.last_read.as_deref()), (0, Some("20")));
}

#[test]
fn marking_a_chat_unread_sets_a_badge_and_tells_telegram() {
    let world = World::signed_in();
    world.connected();
    let crew = world.until("the group on the list", || world.conversation("g8001"));
    assert_eq!(crew.unread, 0);

    assert_eq!(world.ask(Request::MarkUnread { conversation: key("g8001"), message_id: String::new() }), Response::Ok);
    world.until("the unread mark to be told", || world.calls().iter().any(|call| call == "MarkDialogUnread g8001 unread").then_some(()));
    assert_eq!(world.conversation("g8001").unwrap().unread, 1);

    // Another device takes the mark off. Nothing in the chat is unread, so the badge goes.
    world.steer("unread-mark off");
    world.until("the mark to clear", || world.conversation("g8001").filter(|row| row.unread == 0));

    // The phone puts the mark back on a chat this device had already read.
    world.steer("unread-mark");
    world.until("the mark to show", || world.conversation("g8001").filter(|row| row.unread == 1));

    // Reading it tells Telegram both that the messages were seen and that the mark is off.
    world.ask(Request::MarkRead { conversation: key("g8001"), message_id: "20".into() });
    world.until("the mark to be cleared on the network", || {
        let calls = world.calls();
        (calls.iter().any(|call| call == "ReadHistory g8001 20") && calls.iter().any(|call| call == "MarkDialogUnread g8001 read")).then_some(())
    });
    assert_eq!(world.conversation("g8001").unwrap().unread, 0);
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
        world.ask(Request::Typing { conversation: key("u7003"), activity: protocol::Compose::Typing });
    }
    world.until("typing to be told", || (world.called("SetTyping u7003") == 1).then_some(()));
    world.ask(Request::MarkRead { conversation: key("g8001"), message_id: "20".into() });
    world.ask(Request::MarkRead { conversation: key("c9001"), message_id: "5".into() });
    world.until("reading to be told", || world.calls().contains(&"channels.ReadHistory c9001 5".to_owned()).then_some(()));
    sleep(Duration::from_millis(200));
    assert_eq!(world.called("SetTyping u7003"), 1);
    // g8001 was already read up to 20, so there was nothing to tell for it. Being here is said
    // once at connect, before any acting, and never again: acting's own here() is throttled.
    let calls = world.calls();
    let connect_here = calls.iter().position(|call| call == "UpdateStatus");
    let acting = calls.iter().position(|call| call.starts_with("SetTyping u7003"));
    assert!(connect_here.is_some_and(|at| acting.is_some_and(|last| at < last)), "the connect must say we are here before acting does");
}

#[test]
fn a_voice_recording_is_told_as_a_recording_and_heard_as_one() {
    let world = World::signed_in();
    world.connected();
    let watch = || match world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), people: vec![key("u7002")], ..Watching::default() })) {
        Response::Watching { typists, .. } => typists,
        other => panic!("{other:?}"),
    };
    world.steer("record-audio");
    let typists = world.until("a voice recording", || Some(watch()).filter(|typists| !typists.is_empty()));
    assert_eq!(typists[0].activity, protocol::Compose::RecordingVoice);

    // Incoming round-video, upload-photo, sticker, and speaking-in-call activities map cleanly.
    for (steer, expected) in [
        ("record-round", protocol::Compose::RecordingVideo),
        ("upload-photo", protocol::Compose::UploadingPhoto),
        ("choose-sticker", protocol::Compose::ChoosingSticker),
        ("speaking", protocol::Compose::Speaking),
    ] {
        world.steer(steer);
        let got = world.until(steer, || {
            let list = watch();
            list.first().filter(|t| t.activity == expected).cloned()
        });
        assert_eq!(got.activity, expected);
    }

    // Outgoing: explicit Typing(RecordingVoice) is sent and paced, and Cancel clears it immediately.
    assert_eq!(
        world.ask(Request::Typing { conversation: key("u7003"), activity: protocol::Compose::RecordingVoice }),
        Response::Ok
    );
    world.until("the recording to be told", || (world.called("SetTyping u7003 record-audio") == 1).then_some(()));
    assert_eq!(
        world.ask(Request::Typing { conversation: key("u7003"), activity: protocol::Compose::Cancel }),
        Response::Ok
    );
    world.until("the cancel to be told", || (world.called("SetTyping u7003 cancel") == 1).then_some(()));

    // Starting the recorder sends record-audio for the Telegram chat, and discarding sends cancel.
    assert_eq!(
        world.ask(Request::Record(protocol::RecordRequest::Start { conversation: key("u7002"), input: None, video: false })),
        Response::Ok
    );
    world.until("recorder start to send record-audio", || (world.called("SetTyping u7002 record-audio") >= 1).then_some(()));
    assert_eq!(world.ask(Request::Record(protocol::RecordRequest::Forget)), Response::Ok);
    world.until("recorder discard to send cancel", || (world.called("SetTyping u7002 cancel") >= 1).then_some(()));
}

#[test]
fn reactions_follow_each_chats_own_rules_and_what_they_refuse_never_reaches_telegram() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("g8001", "20"));
    let rules = |id: &str| match world.ask(Request::Reactions { conversation: key(id) }) {
        Response::Reactions { rules, .. } => rules,
        other => panic!("no rules for {id}: {other:?}"),
    };
    // A person: Telegram's whole list but what needs Premium, one reaction each, replaced.
    let person = rules("u7002");
    assert_eq!((person.set, person.only.clone(), person.mine, person.replaces, person.distinct), (ReactionSet::Only, vec!["👍".to_owned(), "❤".into(), "🔥".into(), "🎉".into(), "🚫".into()], 1, true, 11));
    // The group's admins picked three; the channel turned them off; the forum caps a message
    // at two different ones.
    assert_eq!(rules("g8001").only, ["👍", "❤", "🔥"]);
    assert_eq!(rules("c9001").set, ReactionSet::Off);
    assert_eq!(rules("c9002").distinct, 2);
    assert_eq!(world.called("GetAvailableReactions"), 1, "the list is asked once a connection");
    assert!(world.calls().iter().any(|call| call.starts_with("GetFullChannel c9001")));

    let react = |id: &str, message: &str, emoji: &str| world.ask(Request::React { conversation: key(id), message_id: message.into(), emoji: emoji.into(), on: true , burst: false });
    let refused = |answer: Response| match answer {
        Response::Error { code: ErrorCode::Refused, detail } => detail,
        other => panic!("not refused: {other:?}"),
    };
    assert_eq!(refused(react("g8001", "20", "🎉")), "That reaction isn't allowed here.");
    assert_eq!(refused(react("c9001", "5", "👍")), "Reactions are turned off in this chat.");
    assert_eq!(refused(react("u7002", "12", "🦄")), "That reaction isn't allowed here.", "Premium's");
    assert!(!world.calls().iter().any(|call| call.starts_with("SendReaction")), "{:?}", world.calls());
    assert!(world.message("g8001", "20").unwrap().reactions.is_empty(), "nothing was shown either");

    // Without Premium a second reaction replaces the first, on screen and on Telegram.
    assert_eq!(react("g8001", "20", "👍"), Response::Ok);
    assert_eq!(react("g8001", "20", "❤️"), Response::Ok);
    let calls: Vec<String> = world.calls().into_iter().filter(|call| call.starts_with("SendReaction g8001")).collect();
    assert_eq!(calls, ["SendReaction g8001 20 👍", "SendReaction g8001 20 ❤"], "as Telegram spells it");
    let shown: Vec<(String, bool)> = world.message("g8001", "20").unwrap().reactions.into_iter().map(|have| (have.emoji, have.mine)).collect();
    assert_eq!(shown.iter().filter(|(_, mine)| *mine).count(), 1, "{shown:?}");
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

    assert_eq!(
        world.ask(Request::Edit { conversation: key("u7002"), message_id: "101".into(), text: "See you at seven".into(), spans: Vec::new(), suppress_embeds: false, keep_attachments: None }),
        Response::Ok
    );
    let edited = world.message("u7002", "101").unwrap();
    assert!(edited.edited && edited.text == "See you at seven");

    // A reaction shows at once and is what Telegram then says; a refused one is taken back.
    assert_eq!(world.ask(Request::React { conversation: key("u7002"), message_id: "12".into(), emoji: "👍".into(), on: true , burst: false }), Response::Ok);
    let ours = world.message("u7002", "12").unwrap().reactions;
    assert_eq!((ours.len(), ours[0].emoji.as_str(), ours[0].count, ours[0].mine), (1, "👍", 1, true));
    let refused = world.ask(Request::React { conversation: key("u7002"), message_id: "12".into(), emoji: "🚫".into(), on: true , burst: false });
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "That reaction isn't allowed here.".into() });
    assert!(world.message("u7002", "12").unwrap().reactions.iter().all(|reaction| reaction.emoji != "🚫"));

    assert_eq!(world.ask(Request::Delete { conversation: key("u7002"), message_id: "101".into(), scope: protocol::DeleteScope::Everyone }), Response::Ok);
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
    assert_eq!(
        (sections.len(), sections[0].name.as_str(), sections[0].count, members.len(), members[0].name.as_str(), members[0].relation),
        (1, "Members", 3, 3, "Odalys Brandt", Relation::Contact),
    );
}

#[test]
fn a_supergroup_lists_its_owner_admins_and_bots_and_a_search_filters() {
    let world = World::signed_in();
    world.connected();
    let Response::Members { members, sections, .. } = world.ask(Request::Members { conversation: key("c9002"), offset: 0, limit: 50 }) else {
        panic!("no members");
    };
    let names: Vec<&str> = sections.iter().map(|section| section.name.as_str()).collect();
    assert_eq!(names, ["Owner", "Admins", "Bots"]);
    assert_eq!(sections.iter().map(|section| section.count).collect::<Vec<_>>(), [1, 2, 1]);
    assert_eq!(
        members.iter().map(|person| (person.name.as_str(), person.title.as_str(), person.bot)).collect::<Vec<_>>(),
        [("Mira Kessler", "", false), ("Odalys Brandt", "Quartermaster", false), ("Tobin Reyes", "Guide", false), ("Trail Bot", "", true)],
    );
    let calls = world.calls();
    assert!(calls.iter().any(|call| call == "GetParticipants c9002 admins"), "{calls:#?}");
    assert!(calls.iter().any(|call| call == "GetParticipants c9002 bots"), "{calls:#?}");

    world.watch(Watching { conversation: Some(key("c9002")), ..Watching::default() });
    world.ask(Request::People { network: Some(Network::Telegram), relation: None, query: Some("nia".into()), offset: 0, limit: 20 });
    world.until("the member search", || world.calls().iter().any(|call| call == "GetParticipants c9002 search nia").then_some(()));
    let found = world.until("the member the search named", || match world.ask(Request::People {
        network: Some(Network::Telegram), relation: None, query: Some("nia".into()), offset: 0, limit: 20,
    }) {
        Response::People { people, .. } => people.into_iter().find(|person| person.key.id == "u7012"),
        _ => None,
    });
    assert_eq!(found.name, "Nia Pell");
}

#[test]
fn a_group_that_hides_its_members_says_so() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_HIDDEN", "1")]);
    world.connected();
    assert_eq!(
        world.ask(Request::Members { conversation: key("c9002"), offset: 0, limit: 50 }),
        Response::Error { code: ErrorCode::Refused, detail: "This group hides its member list.".into() },
    );
}

#[test]
fn a_free_account_is_told_when_a_send_needs_premium_and_reads_its_limits() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_CAPTION", "2048"), ("TINYCLIENT_TELEGRAM_PEER_PREMIUM", "7002")]);
    world.connected();
    let (accounts, _) = world.status();
    assert_eq!(accounts[0].name, "Mira Kessler");
    assert_eq!(accounts[0].can.caption_limit, 2_048);
    assert_eq!(accounts[0].can.max_file_bytes, 2_000 * 1024 * 1024);

    world.send("u7002", "5002", "premium file", None);
    let failed = world.until("the premium refusal", || {
        world.messages("u7002").into_iter().find(|message| matches!(message.state, SendState::Failed { .. }))
    });
    assert_eq!(failed.state, SendState::Failed { reason: "This needs Telegram Premium.".into() });

    let people = world.until("the contacts", || {
        let asked = Request::People { network: Some(Network::Telegram), relation: Some(Relation::Contact), query: None, offset: 0, limit: 50 };
        match world.ask(asked) {
            Response::People { people, .. } if !people.is_empty() => Some(people),
            _ => None,
        }
    });
    assert_eq!((people[0].name.as_str(), people[0].status.as_str()), ("Odalys Brandt", "Telegram Premium"));

    let me = world.until("our row", || match world.ask(Request::People {
        network: Some(Network::Telegram), relation: None, query: Some("Mira".into()), offset: 0, limit: 20,
    }) {
        Response::People { people, .. } => people.into_iter().find(|person| person.key.id == "u7001"),
        _ => None,
    });
    assert_eq!(me.link, "tg://premium_offer");
    assert!(me.status.is_empty(), "our own status text stays ours to set");
}

#[test]
fn a_public_username_resolves_a_person_outside_contacts_and_global_search_results() {
    let world = World::signed_in();
    world.connected();
    let request = || Request::People {
        network: Some(Network::Telegram), relation: None, query: Some("@forest_wren".into()), offset: 0, limit: 20,
    };
    let Response::People { people, .. } = world.ask(request()) else { panic!("no people response") };
    assert!(people.is_empty());
    let person = world.until("the public username to resolve", || match world.ask(request()) {
        Response::People { people, .. } => people.into_iter().find(|p| p.key.id == "u7011"),
        _ => None,
    });
    assert_eq!((person.name.as_str(), person.handle.as_str(), person.relation), ("Robin Vale", "@forest_wren", Relation::None));
    let calls = world.calls();
    let search = calls.iter().position(|call| call == "contacts.Search @forest_wren").unwrap();
    let resolve = calls.iter().position(|call| call == "ResolveUsername forest_wren").unwrap();
    assert!(search < resolve);
    assert_eq!(calls.iter().filter(|call| *call == "contacts.Search @forest_wren").count(), 1);
    // Cache lookup strips the explicit lookup marker, matching both handles and names.
    for query in ["@rob", "@forest_wren"] {
        let Response::People { people, total, .. } = world.ask(Request::People {
            network: Some(Network::Telegram), relation: None, query: Some(query.into()), offset: 0, limit: 20,
        }) else { panic!("no people response") };
        assert_eq!(total, 1);
        assert_eq!(people[0].key.id, "u7011");
    }
    let Response::Conversation { conversation } = world.ask(Request::StartDirect { person: person.key }) else { panic!("no direct chat") };
    assert_eq!(conversation.key.id, "u7011");
}

#[test]
fn username_resolution_requires_an_explicit_valid_handle_and_skips_flood_waits() {
    let world = World::signed_in();
    world.connected();
    for query in ["ro", "rob", "robin", "forest_wren", "@ro", "@robin-vale", "@1robin", "@@robin", "@abcdefghijklmnopqrstuvwxyzabcdefg", "@flood_wait", "@missing_user"] {
        world.ask(Request::People {
            network: Some(Network::Telegram), relation: None, query: Some(query.into()), offset: 0, limit: 20,
        });
        world.within(Duration::from_secs(5), "search without waiting out a flood", || {
            world.calls().contains(&format!("contacts.Search {query}")).then_some(())
        });
    }
    // A later send proves the optional lookup did not stall the command thread.
    world.send("u7002", "6099", "after lookup", None);
    world.within(Duration::from_secs(5), "send after lookup", || {
        world.messages("u7002").into_iter().find(|m| m.text == "after lookup" && m.state == SendState::Sent)
    });
    let calls = world.calls();
    let resolves: Vec<_> = calls.iter().filter(|call| call.starts_with("ResolveUsername ")).map(String::as_str).collect();
    assert_eq!(resolves, ["ResolveUsername flood_wait", "ResolveUsername missing_user"]);
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
fn a_channel_shows_its_sponsored_message_and_tells_telegram_once_it_is_on_screen() {
    let world = World::signed_in();
    world.connected();
    let ad = world.until("the sponsored message", || world.messages("c9001").into_iter().find(|message| message.id == "adad01"));
    assert_eq!(ad.card.as_ref().map(|card| (card.title.as_str(), card.footer.as_str())), Some(("Sponsored", "Larch Boots GmbH")));
    assert_eq!((ad.sender.name.as_str(), ad.text.as_str()), ("Larch Boots", "Resoled by hand in Innsbruck.\nOpen site"));
    assert_eq!(ranges(&ad), vec![("Open site", &Style::Link { url: "https://example.org/boots".into() })]);
    // Read, but not on screen: Telegram is not told it was viewed.
    sleep(Duration::from_millis(300));
    assert_eq!((world.called("GetSponsoredMessages"), world.called("ViewSponsoredMessage")), (1, 0));
    // On screen: told once, however often the channel is shown or opened again.
    let open = || world.watch(Watching { conversation: Some(key("c9001")), ..Watching::default() });
    open();
    world.until("Telegram to be told it was shown", || (world.called("ViewSponsoredMessage") == 1).then_some(()));
    open();
    world.watch(Watching::default());
    open();
    world.messages("c9001");
    sleep(Duration::from_millis(400));
    // Asked again within five minutes, the one already asked is kept.
    assert_eq!((world.called("GetSponsoredMessages"), world.called("ViewSponsoredMessage")), (1, 1));
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
fn following_a_sponsored_message_tells_telegram_of_the_click_but_a_post_does_not() {
    let world = World::signed_in();
    world.connected();
    let ad = world.until("the sponsored message", || world.messages("c9001").into_iter().find(|message| message.id == "adad01"));
    assert_eq!(ad.card.as_ref().map(|card| (card.title.as_str(), card.sponsored)), Some(("Sponsored", true)));
    // The link is followed: Telegram hears of the click.
    assert_eq!(world.ask(Request::FollowLink { conversation: key("c9001"), message_id: "adad01".into() }), Response::Ok);
    world.until("Telegram to be told of the click", || (world.called("ClickSponsoredMessage") == 1).then_some(()));
    // A post's id names no ad: the follow opens but Telegram does not hear of it.
    assert_eq!(world.ask(Request::FollowLink { conversation: key("c9001"), message_id: "5".into() }), Response::Ok);
    // Nor one outside a broadcast channel, ad-shaped as it is.
    assert_eq!(world.ask(Request::FollowLink { conversation: key("u7002"), message_id: "adad01".into() }), Response::Ok);
    sleep(Duration::from_millis(300));
    assert_eq!(world.called("ClickSponsoredMessage"), 1);
    // Every follow is told as its own click.
    assert_eq!(world.ask(Request::FollowLink { conversation: key("c9001"), message_id: "adad01".into() }), Response::Ok);
    world.until("the second click told", || (world.called("ClickSponsoredMessage") == 2).then_some(()));
}

#[test]
fn a_hidden_last_seen_reads_in_words_and_common_groups_are_listed() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { people: vec![key("u7003")], ..Watching::default() });
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7003"), space: None }) else {
        panic!("no profile");
    };
    assert_eq!(profile.person.presence, Presence::Unknown);
    assert_eq!(profile.person.status, "last seen recently");
    assert_eq!(profile.mutual_spaces, vec!["Trail crew".to_owned(), "Workshop".to_owned()]);
    assert!(profile.fields.iter().any(|(label, value)| label == "Birthday" && value == "14 March"));
    assert!(profile.fields.iter().any(|(label, value)| label == "Location" && value == "The hut"));
    assert!(profile.fields.iter().any(|(label, value)| label == "Hours" && value == "Open now · Europe/Lisbon"));
    assert!(profile.fields.iter().any(|(label, value)| label == "Also" && value.contains("@tobin_r")));
    world.steer("rename");
    // The list matches a prefix. "Hale" matches neither the old name nor the new one.
    let renamed = world.until("the new name", || match world.ask(Request::People {
        network: Some(Network::Telegram),
        relation: None,
        query: Some("Tobin Hale".into()),
        offset: 0,
        limit: 20,
    }) {
        Response::People { people, .. } => people.into_iter().find(|person| person.key.id == "u7003"),
        _ => None,
    });
    assert_eq!(renamed.name, "Tobin Hale");
}

#[test]
fn the_panes_ask_telegram_once_each_and_counts_are_telegrams_own() {
    let world = World::signed_in();
    world.connected();
    // A card: the bio, the phone masked, the username; in a group, the admin title there.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7002"), space: None }) else {
        panic!("no profile");
    };
    assert_eq!(profile.bio, "Maps, mostly.");
    assert_eq!(profile.contacts.iter().map(|contact| (contact.value.as_str(), contact.masked)).collect::<Vec<_>>(), [("+15550100777", true)]);
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7003"), space: Some(key("c9002")) }) else {
        panic!("no profile in the forum");
    };
    assert_eq!((profile.nickname.as_str(), profile.joined), ("Quartermaster", Some(t0() - 1_000)));
    assert_eq!(world.called("GetParticipant u7003"), 1);
    // A scam account's card warns.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7005"), space: None }) else {
        panic!("no profile");
    };
    assert_eq!(profile.warning, Some(protocol::Warning::Scam));
    // An impostor's card warns in Telegram's other word.
    let Response::Profile { profile } = world.ask(Request::Profile { person: key("u7006"), space: None }) else {
        panic!("no profile");
    };
    assert_eq!(profile.warning, Some(protocol::Warning::Fake));

    // A channel's info: its own about text and Telegram's counts.
    let Response::ConversationInfo { info } = world.ask(Request::ConversationInfo { conversation: key("c9001") }) else {
        panic!("no info");
    };
    assert_eq!((info.description.as_str(), info.members, info.online, info.link.as_str()), ("Notes from the high country.", Some(1_204), Some(230), "https://t.me/fieldnotes"));
    assert_eq!(info.notify, protocol::Notify::Nothing);

    // Mute is Telegram's, then shown.
    assert_eq!(world.ask(Request::SetMute { conversation: key("g8001"), notify: protocol::Notify::Nothing, until: None }), Response::Ok);
    assert_eq!(world.called("UpdateNotifySettings g8001 2147483647"), 1);
    assert!(world.conversation("g8001").unwrap().muted);
    assert_eq!(world.ask(Request::SetMute { conversation: key("g8001"), notify: protocol::Notify::All, until: None }), Response::Ok);
    assert!(!world.conversation("g8001").unwrap().muted);

    // Pins and shared things from Telegram's own search, with its count as the total.
    let Response::Pinned { messages, .. } = world.ask(Request::Pinned { conversation: key("u7002") }) else {
        panic!("no pins");
    };
    assert_eq!(messages.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), ["12"]);
    world.steer("photo");
    world.arrived("u7002", "14");
    let shared = |kind| match world.ask(Request::Shared { conversation: key("u7002"), kind, before: None, limit: 30, search: true }) {
        Response::Shared { items, total, complete, .. } => (items, total, complete),
        other => panic!("{other:?}"),
    };
    let (items, total, complete) = shared(protocol::SharedKind::Media);
    assert_eq!((items.len(), total, complete), (1, Some(37), true));
    assert_eq!(items[0].attachment.as_ref().map(|file| file.file.id.as_str()), Some("p6014y"));
    let (links, total, _) = shared(protocol::SharedKind::Links);
    assert_eq!((links[0].url.as_str(), total), ("https://example.org/route", Some(4)));

    // Leaving a channel is Telegram's, and the row goes.
    assert_eq!(world.ask(Request::Leave { conversation: key("c9001"), silent: false }), Response::Ok);
    assert_eq!(world.called("LeaveChannel c9001"), 1);
    assert!(world.conversation("c9001").is_none());
    // A direct chat cannot be left.
    assert!(matches!(world.ask(Request::Leave { conversation: key("u7002"), silent: false }), Response::Error { .. }));
}

#[test]
fn a_mute_until_a_time_is_sent_and_a_past_one_does_not_mute() {
    let world = World::signed_in();
    world.connected();
    let until = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 3_600;
    assert_eq!(
        world.ask(Request::SetMute { conversation: key("g8001"), notify: protocol::Notify::Nothing, until: Some(until) }),
        Response::Ok
    );
    assert_eq!(world.called(&format!("UpdateNotifySettings g8001 {until}")), 1);
    assert!(world.conversation("g8001").unwrap().muted);
    assert_eq!(
        world.ask(Request::SetMute { conversation: key("u7003"), notify: protocol::Notify::Nothing, until: Some(1) }),
        Response::Ok
    );
    assert_eq!(world.called("UpdateNotifySettings u7003 1"), 1);
    assert!(!world.conversation("u7003").unwrap().muted);
}

#[test]
fn a_photo_album_is_one_message() {
    let world = World::signed_in();
    world.connected();
    // The album has to be in the chat before the first open. A newest-page read is not asked
    // again for a minute, so a later steer would sit unread.
    world.steer("album");
    let ids = world.until("four photos in history are one grid", || {
        let messages = world.messages("u7002");
        let album = messages.iter().find(|message| message.text == "Ridge in four" && message.attachments.len() == 4)?;
        if album.id != "40" || messages.iter().any(|message| matches!(message.id.as_str(), "41" | "42" | "43")) {
            return None;
        }
        Some(album.attachments.iter().map(|file| file.file.id.clone()).collect::<Vec<_>>())
    });
    assert_eq!(ids, vec!["p6040y", "p6041y", "p6042y", "p6043y"]);

    world.ask(Request::Watch(Watching { conversation: Some(key("u7002")), ..Watching::default() }));
    world.steer("album-live");
    let live = world.until("four photos arriving are one grid", || {
        world.message("u7002", "60").filter(|message| message.attachments.len() == 4 && message.text == "Ridge in four")
    });
    assert!(world.message("u7002", "61").is_none());
    assert!(world.message("u7002", "62").is_none());
    assert!(world.message("u7002", "63").is_none());
    assert_eq!(live.attachments.iter().map(|file| file.file.id.as_str()).collect::<Vec<_>>(), ["p6060y", "p6061y", "p6062y", "p6063y"]);

    world.steer("album-drop");
    world.until("removing one photo leaves the grid", || {
        world.message("u7002", "60").filter(|message| message.attachments.len() == 3 && message.text == "Ridge in four")
    });

    world.steer("album-react");
    let reacted = world.until("a reaction on one photo lands on the grid", || {
        world.message("u7002", "60").filter(|message| message.reactions.iter().any(|reaction| reaction.emoji == "👍"))
    });
    assert_eq!(reacted.attachments.len(), 3);
}

#[test]
fn an_album_cut_by_a_page_is_still_one_grid() {
    let world = World::signed_in();
    world.connected();
    world.steer("album-split");
    let album = world.until("an album split by a page boundary is still one grid", || {
        world.messages("u7002").into_iter().find(|message| {
            message.attachments.len() == 4 && message.text == "Ridge in four" && message.id == "98"
        })
    });
    assert_eq!(album.attachments.iter().map(|file| file.file.id.as_str()).collect::<Vec<_>>(), ["p6098y", "p6099y", "p6100y", "p6101y"]);
    assert!(world.message("u7002", "99").is_none());
    assert!(world.message("u7002", "100").is_none());
    assert!(world.message("u7002", "101").is_none());
}

#[test]
fn pickers_and_sending_with_files_options_and_picks() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    let catalog = |kind, query: &str, cursor: Option<&str>| match world.ask(Request::Catalog { account: account(), kind, space: None, query: query.into(), cursor: cursor.map(str::to_owned) }) {
        Response::Catalog { items, cursor } => (items, cursor),
        other => panic!("{other:?}"),
    };
    // Saved GIFs, the @gif bot's results for a query, the installed packs a pack a page; every
    // preview a still.
    let (gifs, _) = catalog(protocol::CatalogKind::Gifs, "", None);
    assert_eq!(gifs.iter().map(|gif| (gif.id.as_str(), gif.animated)).collect::<Vec<_>>(), [("d6200", true)]);
    assert_eq!(gifs[0].file.as_ref().map(|still| (still.id.as_str(), still.kind)), Some(("d6200m", protocol::MediaKind::Image)));
    let (found, next) = catalog(protocol::CatalogKind::Gifs, "hike", None);
    assert_eq!((found[0].id.as_str(), next.as_deref()), ("i77:r1", Some("2")));
    assert_eq!((world.called("ResolveUsername gif"), world.called("GetInlineBotResults hike")), (1, 1));
    let (stickers, next) = catalog(protocol::CatalogKind::Stickers, "", None);
    assert_eq!(stickers.iter().map(|sticker| (sticker.name.as_str(), sticker.pack.as_str())).collect::<Vec<_>>(), [("🥾", "Hiking"), ("🥾", "Hiking")]);
    assert_eq!(next, None);

    let send = |client_id: &str, text: &str, with: protocol::Outgoing| {
        let asked = Request::Send { conversation: key("u7002"), client_id: client_id.into(), text: text.into(), reply_to: None, with };
        assert!(matches!(world.ask(asked), Response::Sent { .. }));
        world.until(&format!("send {client_id}"), || {
            world.messages("u7002").into_iter().find(|message| message.client_id.as_deref() == Some(client_id) && message.state != SendState::Pending)
        })
    };
    let pick = |id: &str| protocol::Outgoing { pick: Some(protocol::Pick { kind: protocol::CatalogKind::Gifs, id: id.into() }), ..Default::default() };
    // A pick uploads nothing.
    assert_eq!(send("8101", "", pick("d6200")).state, SendState::Sent);
    assert_eq!(world.called("SendMedia u7002 gif"), 1);
    assert_eq!(send("8102", "", pick("i77:r1")).state, SendState::Sent);
    assert_eq!(world.called("SendInlineBotResult u7002 77 r1"), 1);
    assert_eq!(world.called("SaveFilePart"), 0);

    // Silent and scheduled are Telegram's to carry out.
    let silent = send("8103", "hush", protocol::Outgoing { silent: true, ..Default::default() });
    assert_eq!(silent.state, SendState::Sent);
    assert_eq!(world.called("SendMessage u7002 hush [silent]"), 1);
    // ✓✓: Odalys read my message.
    let id: i32 = silent.id.parse().unwrap();
    assert!(!world.conversation("u7002").unwrap().seen);
    world.steer(&format!("outbox {id}"));
    world.until("the second tick", || world.conversation("u7002").filter(|row| row.seen && row.preview_mine));
    // Scheduled: Telegram holds it and sends it then; until then it is shown under its own id.
    let scheduled = send("8104", "later on", protocol::Outgoing { schedule: Some(1_893_456_000), ..Default::default() });
    assert_eq!((scheduled.state, scheduled.id.starts_with('s')), (SendState::Sent, true));
    assert_eq!(world.called("SendMessage u7002 later on [scheduled]"), 1);

    // Files, from their paths, in 512 KB parts: one photo with a spoiler; then two photos and
    // a document sent as a file, an album of the photos and the document on its own.
    // On Windows "/tmp" has no drive; chatd takes only absolute paths.
    let dir = std::path::absolute(format!("/tmp/tinyclient-tg-files-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let file = |name: &str, bytes: usize| {
        let path = dir.join(name);
        std::fs::write(&path, vec![7u8; bytes]).unwrap();
        path.to_string_lossy().into_owned()
    };
    let one = |path: String, spoiler: bool, as_file: bool| protocol::OutgoingFile { path, name: String::new(), spoiler, as_file, original: false, view_once: false, media: None };
    let photo = file("ridge.jpg", 700 * 1024);
    let sent = send("8105", "the ridge", protocol::Outgoing { files: vec![one(photo.clone(), true, false)], ..Default::default() });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert_eq!((world.called("SaveFilePart 0 524288"), world.called("SaveFilePart 1 192512")), (1, 1));
    assert!(world.called("SetTyping u7002 upload-photo") >= 1, "{:?}", world.calls());
    assert_eq!(world.called("SendMedia u7002 photo+spoiler the ridge"), 1);
    let with = vec![one(photo.clone(), false, false), one(file("map.png", 1_000), false, false), one(file("plan.pdf", 2_000), false, true)];
    let album = send("8106", "trip", protocol::Outgoing { files: with, ..Default::default() });
    assert_eq!(album.state, SendState::Sent);
    assert_eq!((world.called("UploadMedia photo"), world.called("SendMultiMedia u7002 kept-photo,kept-photo")), (2, 1));
    let album = world.until("two photos sent together are one grid", || {
        world.messages("u7002").into_iter().find(|message| {
            message.client_id.as_deref() == Some("8106") && message.attachments.len() == 2 && message.text == "trip"
        })
    });
    assert_eq!(album.attachments.len(), 2);
    assert_eq!(world.messages("u7002").iter().filter(|message| message.client_id.as_deref() == Some("8106")).count(), 1);
    // The album is the message, and shows as sent once it lands; the document goes after it.
    world.until("the document after the album", || (world.called("SendMedia u7002 file:application/pdf") > 0).then_some(()));
    assert_eq!(world.called("SendMedia u7002 file:application/pdf"), 1);
    // A big file goes in big parts.
    let big = file("track.gpx", 10 * 1024 * 1024 + 5);
    assert_eq!(send("8107", "", protocol::Outgoing { files: vec![one(big, false, false)], ..Default::default() }).state, SendState::Sent);
    assert_eq!((world.called("SaveBigFilePart 0/21 524288"), world.called("SaveBigFilePart 20/21 5")), (1, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_uninstalled_sticker_can_be_added_and_recent_favourite_and_searched_stickers_show() {
    let stickers = |world: &World, query: &str, cursor: Option<&str>| match world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Stickers,
        space: None,
        query: query.into(),
        cursor: cursor.map(str::to_owned),
    }) {
        Response::Catalog { items, cursor } => (items, cursor),
        other => panic!("{other:?}"),
    };
    let exact = |world: &World, line: &str| world.calls().iter().any(|call| call == line);

    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    let (found, next) = stickers(&world, "camp", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "⛺");
    assert_eq!(found[0].pack, "Camp");
    assert_eq!(found[0].set, "920:921");
    assert!(!found[0].locked);
    assert!(next.is_none());
    assert!(exact(&world, "SearchStickers"), "{:?}", world.calls());

    assert_eq!(
        world.ask(Request::StickerSet { account: account(), set: found[0].set.clone(), install: Some(true), order: Vec::new() }),
        Response::Ok
    );
    assert!(exact(&world, "InstallStickerSet 920"), "{:?}", world.calls());

    let (page, next) = stickers(&world, "", None);
    assert_eq!(page.iter().map(|sticker| (sticker.name.as_str(), sticker.pack.as_str())).collect::<Vec<_>>(), [("🥾", "Hiking"), ("🥾", "Hiking")]);
    assert!(page.iter().all(|sticker| sticker.set.is_empty() && !sticker.locked));
    assert_eq!(next.as_deref(), Some("1"));
    let (page, next) = stickers(&world, "", Some("1"));
    assert_eq!(page.len(), 1, "{page:?}");
    assert_eq!((page[0].name.as_str(), page[0].pack.as_str()), ("⛺", "Camp"));
    assert!(next.is_none());

    assert_eq!(
        world.ask(Request::StickerSet { account: account(), set: String::new(), install: None, order: vec![920, 900] }),
        Response::Ok
    );
    let (page, next) = stickers(&world, "", None);
    assert_eq!(page[0].pack, "Camp");
    assert_eq!(next.as_deref(), Some("1"));

    assert_eq!(
        world.ask(Request::StickerSet { account: account(), set: "920:921".into(), install: Some(false), order: Vec::new() }),
        Response::Ok
    );
    let (page, next) = stickers(&world, "", None);
    assert_eq!(page.iter().map(|sticker| sticker.pack.as_str()).collect::<Vec<_>>(), ["Hiking", "Hiking"]);
    assert!(next.is_none());
    let hiking = page[0].id.clone();

    let (found, _) = stickers(&world, "⛺", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].set, "920:921");
    assert!(exact(&world, "GetStickers"), "{:?}", world.calls());

    assert_eq!(world.ask(Request::FaveSticker { account: account(), id: hiking, unfave: false }), Response::Ok);
    let (page, next) = stickers(&world, "", None);
    assert_eq!(page[0].pack, "Favourites");
    assert_eq!(next.as_deref(), Some("1"));

    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    world.steer("featured-premium");
    let (page, next) = stickers(&world, "", None);
    assert!(!page.is_empty());
    assert!(page.iter().all(|sticker| sticker.locked && sticker.set.is_empty()), "{page:?}");
    assert_eq!(page[0].pack, "Gold");
    assert_eq!(next.as_deref(), Some("1"));
    let refused = world.ask(Request::StickerSet { account: account(), set: "930:931".into(), install: Some(true), order: Vec::new() });
    assert_eq!(refused, Response::Error { code: ErrorCode::Refused, detail: "This needs Telegram Premium.".into() });
    assert!(!exact(&world, "InstallStickerSet 930"), "{:?}", world.calls());
    let (found, _) = stickers(&world, "✨", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].locked && found[0].set.is_empty(), "{found:?}");
    assert_eq!(found[0].pack, "Gold");
    let refused_name = world.ask(Request::StickerSet { account: account(), set: "gold".into(), install: Some(true), order: Vec::new() });
    assert_eq!(refused_name, Response::Error { code: ErrorCode::Refused, detail: "This needs Telegram Premium.".into() });
    assert!(!world.calls().iter().any(|call| call.starts_with("InstallStickerSet")), "{:?}", world.calls());
    let (found, _) = stickers(&world, "gold", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].locked && found[0].set.is_empty(), "{found:?}");

    world.steer("gold-installed");
    let (page, next) = stickers(&world, "", None);
    assert_eq!(page[0].pack, "Hiking");
    assert!(page.iter().all(|sticker| sticker.set.is_empty() && !sticker.locked), "{page:?}");
    assert_eq!(next.as_deref(), Some("1"));
    let (page, next) = stickers(&world, "", Some("1"));
    assert_eq!(page[0].pack, "Gold");
    assert!(page.iter().all(|sticker| sticker.locked && sticker.set.is_empty()), "{page:?}");
    assert!(next.is_none());
    let (found, _) = stickers(&world, "✨", None);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].locked && found[0].set.is_empty(), "{found:?}");

    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    world.steer("recent-stickers");
    let (page, next) = stickers(&world, "", None);
    assert_eq!(page[0].pack, "Recent");
    assert_eq!(next.as_deref(), Some("1"));
}

#[test]
fn an_inline_bot_query_lists_articles_and_a_pick_sends_the_result() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    let (found, next) = match world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Gifs,
        space: Some(key("u7002")),
        query: "@wiki rust".into(),
        cursor: None,
    }) {
        Response::Catalog { items, cursor } => (items, cursor),
        other => panic!("{other:?}"),
    };
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "Rust");
    assert_eq!(found[0].pack, "A language");
    assert!(found[0].file.is_none(), "an article has no file");
    assert_eq!(next.as_deref(), Some("2"));
    assert_eq!(world.called("ResolveUsername wiki"), 1);
    assert_eq!(world.called("GetInlineBotResults rust"), 1);
    assert!(
        world.calls().iter().any(|call| call.contains("7060") && call.contains("u7002")),
        "the query is asked of that bot in the open chat: {:?}",
        world.calls()
    );
    let asked = Request::Send {
        conversation: key("u7002"),
        client_id: "8201".into(),
        text: String::new(),
        reply_to: None,
        with: protocol::Outgoing {
            pick: Some(protocol::Pick { kind: protocol::CatalogKind::Gifs, id: found[0].id.clone() }),
            ..Default::default()
        },
    };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    let sent = world.until("the inline result is sent", || {
        world.messages("u7002").into_iter().find(|message| message.client_id.as_deref() == Some("8201") && message.state != SendState::Pending)
    });
    assert_eq!(sent.state, SendState::Sent);
    assert_eq!(world.called("SendInlineBotResult u7002 88 a1"), 1);
}

/// A send whose file is a voice message this client recorded: uploaded as any file, sent as a
/// voice note whose audio attribute carries the facts the recorder read, and the way the
/// message comes back shows the row as it would on Telegram.
#[test]
fn a_recorded_voice_message_is_sent_as_a_telegram_voice_note() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    let send = |client_id: &str, text: &str, with: protocol::Outgoing| {
        let asked = Request::Send { conversation: key("u7002"), client_id: client_id.into(), text: text.into(), reply_to: None, with };
        assert!(matches!(world.ask(asked), Response::Sent { .. }));
        world.until(&format!("send {client_id}"), || {
            world.messages("u7002").into_iter().find(|message| message.client_id.as_deref() == Some(client_id) && message.state != SendState::Pending)
        })
    };
    let dir = std::path::absolute(format!("/tmp/tinyclient-tg-voice-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("voice-1234.opus");
    std::fs::write(&path, vec![7u8; 2048]).unwrap();
    let one = protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false, view_once: false, media: None };
    // Bars at the widest and shut: both are exact through Telegram's five bits a bar.
    let voice = protocol::VoiceNote { duration: 7_500, waveform: vec![255, 0, 255, 0, 255, 0, 255, 0] };
    let sent = send("8201", "", protocol::Outgoing { files: vec![one], voice: Some(voice), ..Default::default() });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert_eq!(world.called("SaveFilePart 0 2048"), 1, "{:?}", world.calls());
    assert!(world.called("SetTyping u7002 upload-audio") >= 1, "{:?}", world.calls());
    assert_eq!(world.called("SendMedia u7002 voice-note:7"), 1, "the audio attribute, and no file name: {:?}", world.calls());
    let attachment = sent.attachments.first().unwrap();
    assert_eq!((attachment.name.as_str(), attachment.mime.as_str()), ("Voice message", "audio/ogg"));
    assert_eq!(attachment.file.kind, protocol::MediaKind::Audio);
    assert_eq!(attachment.duration, 7);
    assert_eq!(attachment.waveform, vec![255, 0, 255, 0, 255, 0, 255, 0]);
    // The file's own name is not sent, and not shown either.
    assert!(!attachment.name.contains("opus"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_dialog_says_whether_they_read_my_last_word_and_a_groups_read_flips_live() {
    let world = World::signed_in();
    world.connected();
    // Tobin read my newest: his dialog's read-outbox covers it, so the row says ✓✓ at the
    // boot read, with no send and no open of the chat.
    let tobin = world.conversation("u7003").unwrap();
    assert_eq!((tobin.preview_mine, tobin.seen), (true, true), "{tobin:?}");
    // The bot has my newest unread: its dialog's read-outbox stops below it, so ✓ only.
    let bot = world.conversation("u7004").unwrap();
    assert_eq!((bot.preview_mine, bot.seen), (true, false), "{bot:?}");
    // A group whose newest word is not mine says nothing: ✓✓ rides my own last word.
    let group = world.conversation("g8001").unwrap();
    assert_eq!((group.preview_mine, group.seen), (false, false), "{group:?}");

    // My word into the group, then the group reads it while I watch: ✓✓ arrives within
    // seconds, from the update and not from a reading of the dialogs again.
    let dialogs_before = world.called("GetDialogs");
    world.send("g8001", "9001", "Rope is on the truck.", None);
    let said = world.until("the group's echo", || {
        world.messages("g8001").into_iter().find(|said| {
            said.text == "Rope is on the truck." && said.state != SendState::Pending
        })
    });
    assert!(!world.conversation("g8001").unwrap().seen);
    world.steer(&format!("outbox g8001 {}", said.id));
    world.until("the group's read", || {
        world.conversation("g8001").filter(|row| row.seen && row.preview_mine)
    });
    assert_eq!(world.called("GetDialogs"), dialogs_before, "the flip came from the update, not a dialogs read");
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
    world.ask(Request::SignOut { account: account(), erase: false });
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
        Change::SignIn { step: SignInStep::Password { hint, problem: None, .. }, .. } => Some(hint.clone()),
        _ => None,
    });
    assert_eq!(hint, "the old street");
    let wrong = world.sign_in(Some(SignInInput::Password { password: "hunter2".into() }));
    assert_eq!(wrong, SignInStep::Password { hint: "the old street".into(), has_recovery: false, problem: Some(SignInProblem::PasswordInvalid) });
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
    let SignInStep::Code { via: CodeVia::App, length: 5, phone, next: Some(CodeVia::Sms), resend_at, problem: None, .. } = sent else {
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
    assert_eq!(password, SignInStep::Password { hint: "the old street".into(), has_recovery: false, problem: None });
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
    if option_env!("TINYCLIENT_TELEGRAM_API_ID").is_some() {
        return;
    }
    // On Windows "/tmp" has no drive; chatd takes only absolute paths.
    let dir = std::path::absolute(format!("/tmp/tinyclient-tg-{}-plain", std::process::id())).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_chatd")).env("TINYCLIENT_DIR", &dir).env_remove("TINYCLIENT_TELEGRAM_FAKE").stderr(Stdio::null()).spawn().unwrap();
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

#[test]
fn a_login_email_a_new_account_and_password_recovery_does_not_sign_in_and_a_passkey_is_refused() {
    let world = World::start(None, "30");
    world.sign_in(None);
    assert_eq!(world.sign_in(Some(SignInInput::UsePhone)), SignInStep::Phone { problem: None });
    let setup = world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0177".into() }));
    assert!(matches!(setup, SignInStep::EmailSetup { ref phone, problem: None } if phone == "+1 555 010 0177"), "{setup:?}");
    let bad = world.sign_in(Some(SignInInput::Email { email: "not-an-email".into() }));
    assert!(matches!(bad, SignInStep::EmailSetup { problem: Some(SignInProblem::Other { .. }), .. }), "{bad:?}");
    let verify = world.sign_in(Some(SignInInput::Email { email: "mira@example.com".into() }));
    assert!(
        matches!(verify, SignInStep::EmailVerify { ref email_pattern, length: 6, problem: None } if email_pattern == "m***@example.com"),
        "{verify:?}"
    );
    let sent = world.sign_in(Some(SignInInput::EmailCode { code: "654321".into() }));
    assert!(matches!(sent, SignInStep::Code { via: CodeVia::App, length: 5, .. }), "{sent:?}");
    assert_eq!(world.sign_in(Some(SignInInput::Code { code: "24680".into() })), SignInStep::Done { account: Some(account()) });
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("mira@example.com") && !said.contains("654321"), "{said}");

    let world = World::start(None, "30");
    world.sign_in(None);
    world.sign_in(Some(SignInInput::UsePhone));
    world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0188".into() }));
    let signup = world.sign_in(Some(SignInInput::Code { code: "24680".into() }));
    assert!(
        matches!(signup, SignInStep::SignUp { ref phone, ref terms, problem: None } if phone == "+1 555 010 0188" && terms.contains("spam")),
        "{signup:?}"
    );
    let nameless = world.sign_in(Some(SignInInput::SignUp { first_name: "  ".into(), last_name: String::new() }));
    assert!(matches!(nameless, SignInStep::SignUp { problem: Some(_), .. }), "{nameless:?}");
    assert_eq!(
        world.sign_in(Some(SignInInput::SignUp { first_name: "Ada".into(), last_name: "Lovelace".into() })),
        SignInStep::Done { account: Some(account()) }
    );
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("Lovelace") && !said.contains("Ada"), "{said}");

    let world = World::start(None, "30");
    world.sign_in(None);
    world.sign_in(Some(SignInInput::UsePhone));
    world.sign_in(Some(SignInInput::Phone { number: "+1 555 010 0166".into() }));
    let password = world.sign_in(Some(SignInInput::Code { code: "24680".into() }));
    assert!(matches!(password, SignInStep::Password { has_recovery: true, problem: None, .. }), "{password:?}");
    let stayed = world.sign_in(Some(SignInInput::RecoverPassword));
    assert!(matches!(stayed, SignInStep::Password { has_recovery: true, problem: None, .. }), "{stayed:?}");
    let still = world.sign_in(Some(SignInInput::RecoveryCode { code: "135790".into() }));
    assert!(matches!(still, SignInStep::Password { has_recovery: true, problem: None, .. }), "{still:?}");
    assert_eq!(world.called("RequestPasswordRecovery"), 0);
    assert_eq!(world.called("RecoverPassword"), 0);
    assert!(world.status().0.is_empty());
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("135790"), "{said}");

    let world = World::start(None, "30");
    world.sign_in(None);
    let refused = world.sign_in(Some(SignInInput::UsePasskey));
    assert!(matches!(refused, SignInStep::Passkey { ref options, problem: Some(SignInProblem::Other { ref reason }) } if options.is_empty() && reason.contains("passkey domain")), "{refused:?}");
    let again = world.sign_in(Some(SignInInput::Passkey {
        id: "cred".into(),
        raw_id: "raw".into(),
        client_data: "secret-client-data".into(),
        authenticator_data: vec![1],
        signature: vec![2],
        user_handle: "handle".into(),
    }));
    assert!(matches!(again, SignInStep::Passkey { problem: Some(_), .. }), "{again:?}");
    assert_eq!(world.called("InitPasskeyLogin"), 0);
    assert_eq!(world.called("FinishPasskeyLogin"), 0);
    let said = format!("{}{}", world.stderr(), world.calls().join("\n"));
    assert!(!said.contains("secret-client-data"), "{said}");
}

#[test]
fn conversation_search_asks_telegram_with_the_query_and_maps_its_hits() {
    let world = World::signed_in();
    world.connected();
    let search = |query: &str| match world.ask(Request::SearchNetwork {
        conversation: Some(key("g8001")), space: None, query: query.into(), account: None, network: None, filters: protocol::SearchFilters::default(), cursor: None, home: false 
    }) {
        Response::MessageHits { hits, .. } => hits,
        other => panic!("{other:?}"),
    };
    let hits = search("Maps");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].conversation, key("g8001"));
    assert_eq!(hits[0].message_id, "20");
    assert!(hits[0].snippet.contains("Maps"));
    assert!(!hits[0].sender.is_empty());
    assert!(!hits[0].title.is_empty());
    assert_eq!(world.called("SearchText g8001 Maps topic=None"), 1);
    assert!(search("no-such-telegram-message").is_empty());
    assert!(search(" ").is_empty());
}

#[test]
fn conversation_search_keeps_the_telegram_forum_topic_scope() {
    let world = World::signed_in();
    world.connected();
    let Response::MessageHits { hits, .. } = world.ask(Request::SearchNetwork {
        conversation: Some(key("c9002_31")), space: None, query: "wrench".into(), account: None, network: None, filters: protocol::SearchFilters::default(), cursor: None, home: false 
    }) else { panic!("no search hits") };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].conversation, key("c9002_31"));
    assert_eq!(hits[0].message_id, "41");
    assert_eq!(world.called("SearchText c9002 wrench topic=Some(31)"), 1);
}

#[test]
fn markers_are_sent_as_entities_and_an_edit_keeps_the_bold() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    world.send("u7002", "5321", "**hi** _there_", None);
    let sent = world.until("the marked message", || {
        world.messages("u7002").into_iter().find(|message| message.text == "hi there" && message.state == SendState::Sent)
    });
    assert_eq!(ranges(&sent), vec![("hi", &Style::Bold), ("there", &Style::Italic)]);
    assert!(
        world.calls().iter().any(|call| call.contains("SendMessage u7002 hi there") && call.contains("bold:0+2") && call.contains("italic:3+5")),
        "{:?}",
        world.calls()
    );
    assert_eq!(
        world.ask(Request::Edit { conversation: key("u7002"), message_id: sent.id.clone(), text: "hi there".into(), spans: Vec::new(), suppress_embeds: false, keep_attachments: None }),
        Response::Ok
    );
    let edited = world.until("the edit to keep the bold", || world.message("u7002", &sent.id).filter(|message| message.edited));
    assert!(ranges(&edited).iter().any(|(text, style)| *text == "hi" && **style == Style::Bold));
    assert!(
        world.calls().iter().any(|call| call.contains(&format!("EditMessage u7002 {}", sent.id)) && call.contains("bold:0+2")),
        "{:?}",
        world.calls()
    );
}

#[test]
fn a_premium_account_lists_its_emoji_packs_and_sending_one_names_the_document() {
    let world = World::premium();
    world.connected();
    let (accounts, _) = world.status();
    assert_eq!(accounts[0].name, "Mira Kessler · Premium");
    assert_eq!(accounts[0].can.max_file_bytes, 4_000 * 1024 * 1024);
    assert_eq!(accounts[0].can.custom_emoji, EmojiScope::All);
    let Response::Catalog { items, .. } = world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Emoji,
        space: None,
        query: String::new(),
        cursor: None,
    }) else {
        panic!("no catalog");
    };
    let faces = items.iter().find(|item| item.pack == "Faces").expect("the installed pack");
    assert_eq!((faces.id.as_str(), faces.name.as_str()), ("e6402", "\u{2B50}"));
    assert!(items.iter().any(|item| item.id == "e6403" && item.pack == "Recent"), "{items:?}");
    world.send("u7002", "4901", "go :\u{2B50}:", None);
    // A later history read keeps the text and the span. It does not keep the pending id.
    let sent = world.until("the custom emoji to be sent", || {
        world.messages("u7002").into_iter().find(|message| message.text == "go \u{2B50}" && message.state == SendState::Sent)
    });
    assert_eq!(sent.text, "go \u{2B50}");
    assert!(
        ranges(&sent).iter().any(|(text, style)| *text == "\u{2B50}" && matches!(style, Style::Emoji { file } if file.id == "e6402")),
        "{:?}",
        ranges(&sent)
    );
    assert!(
        world.calls().iter().any(|call| call.contains("SendMessage u7002 go \u{2B50}") && call.contains("emoji:3+1:6402")),
        "{:#?}",
        world.calls()
    );

    // Without Premium the same pack is listed, and it is sent only in Saved Messages.
    let plain = World::signed_in();
    plain.connected();
    let Response::Catalog { items, .. } = plain.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Emoji,
        space: None,
        query: String::new(),
        cursor: None,
    }) else {
        panic!("no catalog");
    };
    assert!(items.iter().any(|item| item.id == "e6402"), "{items:?}");
    plain.send("u7002", "4902", "go :\u{2B50}:", None);
    let fallback = plain.until("the fallback in another chat", || {
        plain.messages("u7002").into_iter().find(|message| message.text == "go \u{2B50}" && message.state == SendState::Sent)
    });
    assert!(
        !ranges(&fallback).iter().any(|(_, style)| matches!(style, Style::Emoji { .. })),
        "{:?}",
        ranges(&fallback)
    );
    assert!(
        plain.calls().iter().any(|call| call.contains("SendMessage u7002 go \u{2B50}") && !call.contains("emoji:")),
        "{:#?}",
        plain.calls()
    );
    plain.send("u7001", "4903", "go :\u{2B50}:", None);
    let saved = plain.until("the emoji in Saved Messages", || {
        plain.messages("u7001").into_iter().find(|message| message.text == "go \u{2B50}" && message.state == SendState::Sent)
    });
    assert!(
        ranges(&saved).iter().any(|(text, style)| *text == "\u{2B50}" && matches!(style, Style::Emoji { file } if file.id == "e6402")),
        "{:?}",
        ranges(&saved)
    );
    assert!(
        plain.calls().iter().any(|call| call.contains("SendMessage u7001 go \u{2B50}") && call.contains("emoji:3+1:6402")),
        "{:#?}",
        plain.calls()
    );
}

const LEA: &str = r#"{"user":7101,"name":"Lea Hart","home":2,"dcs":[]}"#;

fn lea() -> AccountKey {
    AccountKey { network: Network::Telegram, account: "7101".into() }
}

fn lea_key(id: &str) -> Key {
    Key { network: Network::Telegram, account: "7101".into(), id: id.into() }
}

/// Two stored sessions, both connected, with the same chat id under each account.
fn both_connected(world: &World) {
    world.until("both Telegram accounts to connect", || {
        let accounts = world.status().0;
        let mira = accounts.iter().any(|row| row.key == account() && row.link == Link::Connected);
        let other = accounts.iter().any(|row| row.key == lea() && row.link == Link::Connected && row.name == "Lea Hart");
        (mira && other).then_some(())
    });
}

#[test]
fn two_saved_sessions_connect_and_removing_one_leaves_the_other() {
    let session = format!("[{SESSION},{LEA}]");
    let world = World::start(Some(&session), "30");
    both_connected(&world);
    let mine = world.conversation("u7002").expect("Mira's chat");
    assert_eq!(mine.key.account, "7001");
    let Response::Conversation { conversation } = world.ask(Request::Conversation { conversation: lea_key("u7002") }) else {
        panic!("Lea's chat is missing");
    };
    assert_eq!(conversation.key.account, "7101");
    assert_ne!(mine.key, conversation.key);
    let left = chatd::adapters::telegram::without(&session, "7101").expect("removing one dropped the session");
    let still = chatd::adapters::telegram::accounts(&left).expect("the session left behind");
    assert_eq!(still.iter().map(|signed| signed.account.as_str()).collect::<Vec<_>>(), ["7001"]);
    world.ask(Request::SignOut { account: lea(), erase: false });
    world.until("Mira to stay signed in", || {
        let accounts = world.status().0;
        let mira = accounts.iter().any(|row| row.key == account() && row.link == Link::Connected);
        let gone = accounts.iter().all(|row| row.key != lea());
        (mira && gone).then_some(())
    });
    assert!(world.conversation("u7002").is_some(), "Mira's chat went with Lea");
}

#[test]
fn a_second_sign_in_keeps_the_telegram_account_already_there() {
    let world = World::signed_in();
    world.connected();
    assert!(matches!(world.sign_in(None), SignInStep::Qr { .. }));
    world.steer_sign_in("scan-other");
    both_connected(&world);
    assert!(world.conversation("u7002").is_some());
    let Response::Conversation { conversation } = world.ask(Request::Conversation { conversation: lea_key("u7002") }) else {
        panic!("the second account has no chat");
    };
    assert_eq!(conversation.key, lea_key("u7002"));
}

#[test]
fn block_sends_contacts_block_and_the_person_reads_blocked() {
    let world = World::signed_in();
    world.connected();
    assert_eq!(world.called("GetContacts"), 1, "{:#?}", world.calls());
    assert_eq!(world.called("contacts.getBlocked"), 1, "{:#?}", world.calls());

    let listed = |relation: Relation| match world.ask(Request::People {
        network: Some(Network::Telegram),
        relation: Some(relation),
        query: None,
        offset: 0,
        limit: 50,
    }) {
        Response::People { people, .. } => people,
        other => panic!("{other:?}"),
    };
    assert!(listed(Relation::Blocked).is_empty());
    assert!(listed(Relation::Contact).iter().any(|person| person.key.id == "u7002"));

    assert_eq!(world.ask(Request::Relate { person: key("u7002"), action: RelateAction::Block }), Response::Ok);
    assert_eq!(world.called("contacts.block u7002"), 1, "{:#?}", world.calls());
    world.until("odalys blocked", || {
        listed(Relation::Blocked)
            .iter()
            .any(|person| person.key.id == "u7002" && person.relation == Relation::Blocked)
            .then_some(())
    });
    assert!(listed(Relation::Contact).iter().all(|person| person.key.id != "u7002"));

    world.ask(Request::People {
        network: Some(Network::Telegram),
        relation: None,
        query: Some("wren".into()),
        offset: 0,
        limit: 20,
    });
    world.until("the search to reach Telegram", || {
        world.calls().contains(&"contacts.Search wren".to_owned()).then_some(())
    });
    assert_eq!(world.ask(Request::Relate { person: key("u7010"), action: RelateAction::Add }), Response::Ok);
    assert!(
        world.calls().iter().any(|call| call.starts_with("contacts.addContact u7010 ")),
        "{:#?}",
        world.calls()
    );
    assert!(listed(Relation::Contact).iter().any(|person| person.key.id == "u7010" && person.relation == Relation::Contact));

    assert_eq!(world.ask(Request::Relate { person: key("u7010"), action: RelateAction::Remove }), Response::Ok);
    assert_eq!(world.called("contacts.deleteContacts u7010"), 1, "{:#?}", world.calls());
    assert!(listed(Relation::Contact).iter().all(|person| person.key.id != "u7010"));

    assert_eq!(world.ask(Request::Relate { person: key("u7002"), action: RelateAction::Block }), Response::Ok);
    assert_eq!(world.called("contacts.unblock u7002"), 1, "{:#?}", world.calls());
    assert!(listed(Relation::Blocked).iter().all(|person| person.key.id != "u7002"));
    assert!(listed(Relation::Contact).iter().any(|person| person.key.id == "u7002"));

    world.steer("peer-blocked 7002");
    world.until("the block update", || {
        listed(Relation::Blocked).iter().any(|person| person.key.id == "u7002").then_some(())
    });
    assert_eq!(world.called("contacts.block u7002"), 1, "an update is not another block request");
}

#[test]
fn a_private_delete_after_reconnect_removes_the_message() {
    let mut world = World::signed_in();
    world.connected();
    world.steer("incoming");
    let arrived = world.until("the incoming message", || world.message("u7002", "13"));
    assert!(arrived.text.contains("dock"));
    world.restart();
    world.connected();
    assert!(world.message("u7002", "13").is_some(), "the message stays until Telegram says it was deleted");
    world.steer("delete");
    world.within(Duration::from_secs(5), "the delete after a reconnect", || world.message("u7002", "13").is_none().then_some(()));
}

#[test]
fn mentioning_someone_with_no_username_names_them_on_the_send() {
    let world = World::signed_in();
    world.connected();
    world.send("u7002", "5341", "see @Tobin Reyes", None);
    let sent = world.until("the name mention to be sent", || {
        world.messages("u7002").into_iter().find(|message| message.text == "see Tobin" && message.state == SendState::Sent)
    });
    assert!(
        ranges(&sent).iter().any(|(text, style)| *text == "Tobin" && matches!(style, Style::Mention { kind: MentionKind::User, id } if id == "u7003")),
        "{:?}",
        ranges(&sent)
    );
    assert!(
        world.calls().iter().any(|call| call.contains("SendMessage u7002 see Tobin") && call.contains("mention:4+5:7003")),
        "{:#?}",
        world.calls()
    );

    world.send("u7002", "5342", "see @Tobin Reyes and @Odalys Brandt", None);
    let both = world.until("the username to stay a handle", || {
        world.messages("u7002").into_iter().find(|message| message.text == "see Tobin and @odalys" && message.state == SendState::Sent)
    });
    assert_eq!(both.text, "see Tobin and @odalys");
    let shown = ranges(&both);
    let mentions = shown.iter().filter(|(_, style)| matches!(style, Style::Mention { .. })).count();
    assert_eq!(mentions, 1, "{shown:?}");
    assert!(
        world.calls().iter().any(|call| {
            call.contains("SendMessage u7002 see Tobin and @odalys") && call.contains("mention:4+5:7003") && call.matches("mention:").count() == 1
        }),
        "{:#?}",
        world.calls()
    );
}

#[test]
fn a_secret_chat_is_a_row_that_says_it_stays_in_the_phone_apps() {
    let world = World::signed_in();
    world.connected();
    world.steer("secret");
    let row = world.until("the secret chat", || world.conversation("e42"));
    assert_eq!(row.title, "Odalys Brandt");
    assert!(row.read_only);
    assert_eq!(row.encryption, protocol::Encryption::Unknown);
    assert!(row.preview.contains("Odalys Brandt") && row.preview.contains("phone apps"), "{}", row.preview);
    assert!(row.topic.contains("phone apps"), "{}", row.topic);
    assert!(row.kind == ConversationKind::Direct);
    let listed = world.home(None, Shelf::Main).into_iter().any(|row| row.key.id == "e42");
    assert!(listed, "the secret chat is a row on the list");
    let notice = world.until("the notice", || {
        world.messages("e42").into_iter().find(|message| message.kind == MessageKind::System)
    });
    assert!(notice.text.contains("phone apps"), "{}", notice.text);
    assert!(!world.calls().iter().any(|call| call.contains("AcceptEncryption")), "{:?}", world.calls());
    // The row is read-only, so the composer never reaches Telegram. The notice on the row is
    // what says the chat stays in the phone apps.
    let refused = world.ask(Request::Send {
        conversation: key("e42"),
        client_id: "5301".into(),
        text: "hello".into(),
        reply_to: None,
        with: Default::default(),
    });
    let Response::Error { code: ErrorCode::Refused, detail } = refused else {
        panic!("{refused:?}");
    };
    assert!(detail.contains("Odalys Brandt"), "{detail}");
    assert!(
        !world.calls().iter().any(|call| call.contains("AcceptEncryption") || call.contains("RequestEncryption") || call.contains("DiscardEncryption")),
        "{:?}",
        world.calls()
    );
    world.steer("secret-gone");
    world.until("the row to leave", || world.conversation("e42").is_none().then_some(()));
    assert!(!world.home(None, Shelf::Main).iter().any(|row| row.key.id == "e42"));
}

fn fetch_path(world: &World, id: &str) -> Option<String> {
    match world.ask(Request::Fetch { account: account(), ids: vec![id.into()], size: None, retry: false, span: None }) {
        Response::Media { mut files } => files.pop().and_then(|file| file.path),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_cdn_file_is_decrypted_and_its_checksum_matches() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("history", || world.message("u7002", "12"));
    world.steer("cdn-file");
    let message = world.arrived("u7002", "30");
    let plain = b"ridge file from the cdn";
    let file = &message.attachments[0].file;
    assert_eq!((file.id.as_str(), file.kind, file.bytes), ("d9300", MediaKind::File, plain.len() as u64));
    let landed = world.until("the cdn file on disk", || fetch_path(&world, "d9300"));
    let bytes = std::fs::read(&landed).unwrap();
    assert_eq!(bytes, plain);
    let calls = world.calls();
    let at = |name: &str| calls.iter().position(|call| call == name).unwrap_or_else(|| panic!("{name} missing in {calls:#?}"));
    let asked = at("GetFile document 9300 at 0");
    let hashes = at("GetCdnFileHashes");
    let again = at("CdnReuploadNeeded");
    let reupload = at("ReuploadCdnFile");
    assert!(asked < hashes && hashes < again && again < reupload, "{calls:#?}");
    assert_eq!(calls.iter().filter(|call| *call == "GetCdnFile").count(), 2, "{calls:#?}");
    assert_eq!(world.called("ExportAuthorization 5"), 0, "{calls:#?}");
}

#[test]
fn a_dropped_large_file_resumes_from_the_part() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("history", || world.message("u7002", "12"));
    world.steer("big-file");
    let message = world.arrived("u7002", "31");
    assert_eq!(message.attachments[0].file.id, "d9400");
    assert_eq!(message.attachments[0].file.bytes, (1024 * 1024 + 64) as u64);
    let landed = world.until("the resumed file on disk", || fetch_path(&world, "d9400"));
    let bytes = std::fs::read(&landed).unwrap();
    let expected: Vec<u8> = (0..(1024 * 1024 + 64)).map(|at| (at % 251) as u8).collect();
    assert_eq!(bytes, expected);
    assert_eq!(world.called("GetFile document 9400 at 0"), 1, "{:#?}", world.calls());
    assert!(world.called("GetFile document 9400 at 1048576") >= 1, "{:#?}", world.calls());
}

#[test]
fn a_link_card_fetches_its_picture_and_not_the_full_photo() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("history", || world.message("u7002", "12"));
    world.steer("link");
    let message = world.arrived("u7002", "32");
    let card = &message.previews[0];
    assert_eq!((card.url.as_str(), card.title.as_str(), card.site.as_str()), ("https://example.org/ridge", "Ridge", "Example"));
    let image = card.image.as_ref().expect("the card names its picture");
    assert_eq!((image.id.as_str(), image.width, image.height), ("p8801m", 320, 240));
    let landed = world.until("the card's picture on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["p8801m".into()], size: None, retry: false, span: None }) {
            Response::Media { mut files } => files.pop().and_then(|file| file.path),
            other => panic!("{other:?}"),
        }
    });
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), FILE_BYTES);
    assert_eq!(world.called("GetFile photo 8801 m"), 2, "{:#?}", world.calls());
    assert_eq!(world.called("GetFile photo 8801 y"), 0, "{:#?}", world.calls());
}

#[test]
fn delete_chat_and_clear_history_loop_over_offset_and_update_store() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.until("u7002 history", || world.message("u7002", "12"));

    // Clear history on a DM with revoke=true: loops over non-zero offset, clears stored messages,
    // and keeps the conversation row.
    assert_eq!(
        world.ask(Request::ClearHistory { conversation: key("u7002"), delete_chat: false, revoke: true }),
        Response::Ok
    );
    assert_eq!(world.called("DeleteHistory u7002 just_clear=true revoke=true max_id=0"), 2, "{:#?}", world.calls());
    assert!(world.conversation("u7002").is_some(), "clear history keeps the chat row");
    assert!(world.messages("u7002").is_empty(), "clear history removes stored messages");

    // Delete chat on a DM: removes the row and its stored messages.
    assert_eq!(
        world.ask(Request::ClearHistory { conversation: key("u7002"), delete_chat: true, revoke: false }),
        Response::Ok
    );
    assert_eq!(world.called("DeleteHistory u7002 just_clear=false revoke=false max_id=0"), 1, "{:#?}", world.calls());
    assert!(world.conversation("u7002").is_none(), "delete chat removes the row");

    // Clear history on a supergroup/channel uses channels.deleteHistory.
    assert_eq!(
        world.ask(Request::ClearHistory { conversation: key("c9001"), delete_chat: false, revoke: false }),
        Response::Ok
    );
    assert_eq!(world.called("ChannelDeleteHistory c9001 for_everyone=false max_id=0"), 1, "{:#?}", world.calls());
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// Builds a minimal valid ISO BMFF (.mp4) with `mvhd` (duration_ms) and `trak`/`tkhd` (width x height).
/// When `moov_first` is true, `moov` precedes `mdat` (fast-start / streamable).
fn sample_mp4(width: u32, height: u32, duration_ms: u32, moov_first: bool) -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\x00\x00\x02\x00isommp41");
    // mvhd version 0: 4 bytes flags/ver, 4 creation, 4 mod, 4 timescale (1000), 4 duration, rest padding (100 bytes total).
    let mut mvhd_payload = vec![0u8; 100];
    mvhd_payload[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd_payload[16..20].copy_from_slice(&duration_ms.to_be_bytes());
    let mvhd = mp4_box(b"mvhd", &mvhd_payload);
    // tkhd version 0: 84 bytes payload; width at 76..80 (16.16 fixed), height at 80..84 (16.16 fixed).
    let mut tkhd_payload = vec![0u8; 84];
    tkhd_payload[76..80].copy_from_slice(&(width << 16).to_be_bytes());
    tkhd_payload[80..84].copy_from_slice(&(height << 16).to_be_bytes());
    let tkhd = mp4_box(b"tkhd", &tkhd_payload);
    let trak = mp4_box(b"trak", &tkhd);
    let mut moov_body = Vec::new();
    moov_body.extend_from_slice(&mvhd);
    moov_body.extend_from_slice(&trak);
    let moov = mp4_box(b"moov", &moov_body);
    let mdat = mp4_box(b"mdat", &[0u8; 32]);
    let mut file = Vec::new();
    file.extend_from_slice(&ftyp);
    if moov_first {
        file.extend_from_slice(&moov);
        file.extend_from_slice(&mdat);
    } else {
        file.extend_from_slice(&mdat);
        file.extend_from_slice(&moov);
    }
    file
}

#[test]
fn sent_video_carries_dimensions_duration_streaming_flag_and_thumb() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));
    let dir = std::path::absolute(format!("/tmp/tinyclient-tg-video-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let fast_path = dir.join("clip.mp4");
    std::fs::write(&fast_path, sample_mp4(1280, 720, 12_500, true)).unwrap();
    let thumb_path = dir.join("poster.jpg");
    std::fs::write(&thumb_path, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
    let slow_path = dir.join("late-moov.mp4");
    std::fs::write(&slow_path, sample_mp4(640, 360, 4_000, false)).unwrap();
    let song_path = dir.join("track.mp3");
    let mut mp3 = vec![0u8; 16];
    let mut id3v1 = vec![0u8; 128];
    id3v1[0..3].copy_from_slice(b"TAG");
    id3v1[3..14].copy_from_slice(b"Night Drive");
    id3v1[33..42].copy_from_slice(b"Synthwave");
    mp3.extend_from_slice(&id3v1);
    std::fs::write(&song_path, mp3).unwrap();

    let send = |client_id: &str, file: protocol::OutgoingFile| {
        assert!(matches!(
            world.ask(Request::Send {
                conversation: key("u7002"),
                client_id: client_id.into(),
                text: String::new(),
                reply_to: None,
                with: protocol::Outgoing { files: vec![file], ..Default::default() },
            }),
            Response::Sent { .. }
        ));
        world.until("sent", || {
            world.messages("u7002").into_iter().find(|m| m.client_id.as_deref() == Some(client_id) && m.state != SendState::Pending)
        })
    };

    let sent = send("8901", protocol::OutgoingFile {
        path: fast_path.to_string_lossy().into_owned(),
        media: Some(protocol::OutgoingMedia {
            thumb: Some(thumb_path.to_string_lossy().into_owned()),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert!(
        world.calls().iter().any(|c| c.contains("SendMedia u7002 video:1280x720:12.5:stream=true:thumb=true")),
        "expected fast-start video attribute + thumb on wire, got: {:#?}",
        world.calls()
    );

    let sent_slow = send("8902", protocol::OutgoingFile {
        path: slow_path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    assert_eq!(sent_slow.state, SendState::Sent, "{:?}", world.calls());
    assert!(
        world.calls().iter().any(|c| c.contains("SendMedia u7002 video:640x360:4.0:stream=false:thumb=false")),
        "expected non-fast-start video to have supports_streaming=false, got: {:#?}",
        world.calls()
    );

    let sent_song = send("8903", protocol::OutgoingFile {
        path: song_path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    assert_eq!(sent_song.state, SendState::Sent, "{:?}", world.calls());
    assert!(
        world.calls().iter().any(|c| c.contains("SendMedia u7002 audio:Night Drive:Synthwave")),
        "expected audio tags on wire, got: {:#?}",
        world.calls()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_user_call_flags_gate_call_support_and_video() {
    let world = World::signed_in();
    world.connected();
    let row = |id: &str| world.conversation(id).expect(id);
    // Before users.getFullUser is asked, both people keep the optimistic hand-off button.
    assert_eq!(row("u7002").call, protocol::CallSupport::Handoff);
    assert!(!row("u7002").call_video);
    assert_eq!(row("u7003").call, protocol::CallSupport::Handoff);

    // 1. phone_calls_available = false, phone_calls_private = false -> CallSupport::None.
    world.steer("call-flags 7002 0 0 0");
    assert!(matches!(world.ask(Request::Profile { person: key("u7002"), space: None }), Response::Profile { .. }));
    assert_eq!(row("u7002").call, protocol::CallSupport::None);
    assert!(!row("u7002").call_video);

    // 2. phone_calls_private = true -> CallSupport::Restricted (explained tooltip, not silently hidden).
    world.steer("call-flags 7003 0 1 0");
    assert!(matches!(world.ask(Request::ConversationInfo { conversation: key("u7003") }), Response::ConversationInfo { .. }));
    assert_eq!(row("u7003").call, protocol::CallSupport::Restricted);
    assert!(!row("u7003").call_video);

    // 3. phone_calls_available = true, video_calls_available = true -> Handoff + call_video.
    world.steer("call-flags 7002 1 0 1");
    assert!(matches!(world.ask(Request::ConversationInfo { conversation: key("u7002") }), Response::ConversationInfo { .. }));
    assert_eq!(row("u7002").call, protocol::CallSupport::Handoff);
    assert!(row("u7002").call_video);
}

#[test]
fn call_service_rows_map_direction_reason_video_and_group_actions() {
    use protocol::CallOutcome;
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));

    // 1. Missed incoming voice call -> Missed, preview "Missed call", raises unread.
    let unread_before = world.conversation("u7002").unwrap().unread;
    world.steer("call-event in missed voice");
    let missed = world.until("missed call", || {
        world.messages("u7002").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Missed))
    });
    assert_eq!(missed.kind, MessageKind::System);
    assert_eq!(missed.text, "Missed call");
    assert!(!missed.outgoing);
    assert!(!missed.call.as_ref().unwrap().video);
    let conv = world.conversation("u7002").unwrap();
    assert_eq!(conv.preview, "Missed call");
    assert!(conv.unread > unread_before, "missed incoming call must count as unread");

    // 2. Cancelled outgoing call (out + missed reason).
    world.steer("call-event out missed voice");
    let cancelled = world.until("cancelled call", || {
        world.messages("u7002").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Cancelled))
    });
    assert!(cancelled.outgoing);
    assert_eq!(cancelled.text, "Cancelled call");

    // 3. Line busy (out + busy reason).
    world.steer("call-event out busy voice");
    let busy = world.until("busy call", || {
        world.messages("u7002").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Busy))
    });
    assert!(busy.outgoing);
    assert_eq!(busy.text, "Line busy");

    // 4. Declined incoming call (in + busy reason).
    world.steer("call-event in busy voice");
    let declined = world.until("declined call", || {
        world.messages("u7002").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Declined))
    });
    assert!(!declined.outgoing);
    assert_eq!(declined.text, "Declined call");

    // 5. Outgoing ended video call with duration (out + hangup + video + 95s).
    world.steer("call-event out hangup video 95");
    let ended_vid = world.until("ended video call", || {
        world.messages("u7002").into_iter().find(|m| {
            m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Ended && c.video && c.seconds == Some(95))
        })
    });
    assert!(ended_vid.outgoing);
    assert!(ended_vid.text.starts_with("Outgoing video call"), "{}", ended_vid.text);

    // 6. Group video chat started (Live) and ended, plus conference call.
    world.steer("call-event in group-live video");
    let group_live = world.until("group live", || {
        world.messages("g8001").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Live))
    });
    assert!(group_live.text.contains("started a video chat"), "{}", group_live.text);

    world.steer("call-event in conf-missed video");
    let conf_missed = world.until("conf missed", || {
        world.messages("g8001").into_iter().find(|m| m.call.as_ref().is_some_and(|c| c.outcome == CallOutcome::Missed))
    });
    assert_eq!(conf_missed.kind, MessageKind::System);
    assert_eq!(conf_missed.text, "Missed group video call");
}

#[test]
fn call_history_pages_filters_missed_and_deletes_entries() {
    use protocol::CallOutcome;
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));

    // Emit 3 calls: outgoing ended, incoming missed, outgoing video ended.
    world.steer("call-event out hangup voice 45");
    world.steer("call-event in missed voice");
    world.steer("call-event out hangup video 120");
    world.until("3 calls stored", || {
        let count = world.messages("u7002").into_iter().filter(|m| m.call.is_some()).count();
        (count >= 3).then_some(())
    });

    // Page 1 (limit 2): newest two calls.
    let Response::CallHistory { entries: p1, next: next1, more: more1 } = world.ask(Request::CallHistory {
        account: account(),
        missed: false,
        before: None,
        limit: 2,
    }) else {
        panic!("expected CallHistory");
    };
    assert_eq!(p1.len(), 2);
    assert!(more1);
    assert!(p1[0].note.video && p1[0].note.seconds == Some(120));
    assert_eq!(p1[1].note.outcome, CallOutcome::Missed);

    // Page 2 using `next1`: remaining call.
    let Response::CallHistory { entries: p2, more: more2, .. } = world.ask(Request::CallHistory {
        account: account(),
        missed: false,
        before: next1,
        limit: 2,
    }) else {
        panic!("expected CallHistory page 2");
    };
    assert_eq!(p2.len(), 1);
    assert!(!more2);
    assert_eq!(p2[0].note.seconds, Some(45));

    // Missed filter returns only the missed incoming call.
    let Response::CallHistory { entries: missed, .. } = world.ask(Request::CallHistory {
        account: account(),
        missed: true,
        before: None,
        limit: 10,
    }) else {
        panic!("expected missed CallHistory");
    };
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].note.outcome, CallOutcome::Missed);

    // Delete with revoke = true removes it on the wire.
    let target_id = missed[0].message_id.clone();
    assert!(matches!(
        world.ask(Request::DeleteCallEntries {
            account: account(),
            message_ids: vec![target_id.clone()],
            revoke: true,
        }),
        Response::Ok
    ));
    assert!(
        world.calls().iter().any(|c| c.contains("DeleteMessages") && c.contains("revoke true")),
        "expected DeleteMessages revoke true, got: {:#?}",
        world.calls()
    );
    let Response::CallHistory { entries: after_del, .. } = world.ask(Request::CallHistory {
        account: account(),
        missed: true,
        before: None,
        limit: 10,
    }) else {
        panic!("expected CallHistory after delete");
    };
    assert!(after_del.is_empty());
}

#[test]
fn privacy_settings_read_and_write_rules_with_user_and_chat_exceptions() {
    use protocol::{PrivacyAudience, PrivacyException, PrivacyKey, PrivacyPolicy};
    let world = World::signed_in();
    world.connected();

    // 1. Read default PhoneCall, PhoneP2p, and VoiceMessages rules.
    let Response::Privacy { policy: call_pol, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::PhoneCall,
    }) else {
        panic!("expected Privacy for PhoneCall");
    };
    assert_eq!(call_pol.audience, PrivacyAudience::Everybody);
    assert!(!call_pol.premium_only);

    let Response::Privacy { policy: p2p_pol, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::PhoneP2p,
    }) else {
        panic!("expected Privacy for PhoneP2p");
    };
    assert_eq!(p2p_pol.audience, PrivacyAudience::Contacts);

    let Response::Privacy { policy: voice_pol, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::VoiceMessages,
    }) else {
        panic!("expected Privacy for VoiceMessages");
    };
    assert!(voice_pol.premium_only, "free account should mark VoiceMessages as premium_only");

    // 2. Write PhoneCall = Contacts with one always-allow user (u7002), one never-allow user (u7003),
    //    and one always-allow chat (g8001).
    let updated = PrivacyPolicy {
        audience: PrivacyAudience::Contacts,
        always_allow: vec![
            PrivacyException { id: "u7002".into(), name: String::new(), chat: false },
            PrivacyException { id: "g8001".into(), name: String::new(), chat: true },
        ],
        never_allow: vec![PrivacyException { id: "u7003".into(), name: String::new(), chat: false }],
        premium_only: false,
    };
    let Response::Privacy { policy: saved, .. } = world.ask(Request::SetPrivacy {
        account: account(),
        key: PrivacyKey::PhoneCall,
        policy: updated,
    }) else {
        panic!("expected Privacy after SetPrivacy");
    };
    assert_eq!(saved.audience, PrivacyAudience::Contacts);
    assert!(saved.always_allow.iter().any(|e| e.id == "u7002" && !e.chat));
    assert!(saved.always_allow.iter().any(|e| e.id == "g8001" && e.chat));
    assert!(saved.never_allow.iter().any(|e| e.id == "u7003" && !e.chat));
    assert!(
        world.calls().iter().any(|c| c.contains("account.setPrivacy phone_call")
            && c.contains("allow_users:[7002]")
            && c.contains("allow_chats:[8001]")
            && c.contains("disallow_users:[7003]")
            && c.contains("allow_contacts")),
        "expected exact setPrivacy rules on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn privacy_last_seen_sets_contacts_with_one_exception_and_a_premium_refusal() {
    use protocol::{ErrorCode, PrivacyAudience, PrivacyException, PrivacyKey, PrivacyPolicy};
    let world = World::signed_in();
    world.connected();

    let Response::Privacy { policy: seen, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::LastSeen,
    }) else {
        panic!("expected Privacy for LastSeen");
    };
    assert_eq!(seen.audience, PrivacyAudience::Everybody);
    assert!(!seen.premium_only);

    let Response::Privacy { policy: phone, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::PhoneNumber,
    }) else {
        panic!("expected Privacy for PhoneNumber");
    };
    assert_eq!(phone.audience, PrivacyAudience::Contacts);

    let Response::Privacy { policy: photo, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::ProfilePhoto,
    }) else {
        panic!("expected Privacy for ProfilePhoto");
    };
    assert_eq!(photo.audience, PrivacyAudience::Everybody);

    let Response::Privacy { policy: invite, .. } = world.ask(Request::GetPrivacy {
        account: account(),
        key: PrivacyKey::ChatInvite,
    }) else {
        panic!("expected Privacy for ChatInvite");
    };
    assert_eq!(invite.audience, PrivacyAudience::Everybody);

    let updated = PrivacyPolicy {
        audience: PrivacyAudience::Contacts,
        always_allow: vec![PrivacyException { id: "u7002".into(), name: String::new(), chat: false }],
        never_allow: Vec::new(),
        premium_only: false,
    };
    let Response::Privacy { policy: saved, .. } = world.ask(Request::SetPrivacy {
        account: account(),
        key: PrivacyKey::LastSeen,
        policy: updated,
    }) else {
        panic!("expected Privacy after SetPrivacy LastSeen");
    };
    assert_eq!(saved.audience, PrivacyAudience::Contacts);
    assert!(saved.always_allow.iter().any(|e| e.id == "u7002" && !e.chat));
    assert!(
        world.calls().iter().any(|c| c.contains("account.setPrivacy status_timestamp")
            && c.contains("allow_users:[7002]")
            && c.contains("allow_contacts")),
        "expected last-seen rules on the wire, got: {:#?}",
        world.calls()
    );

    world.steer("privacy-refuse");
    let refused = world.ask(Request::SetPrivacy {
        account: account(),
        key: PrivacyKey::LastSeen,
        policy: PrivacyPolicy { audience: PrivacyAudience::Nobody, ..PrivacyPolicy::default() },
    });
    match refused {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("Telegram Premium"), "{detail}");
        }
        other => panic!("expected a premium refusal, got {other:?}"),
    }
    assert!(
        world.calls().iter().any(|c| c.contains("account.setPrivacy status_timestamp refused")),
        "expected the refused setPrivacy on the wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn auto_download_reads_wifi_limits_and_refuses_a_save() {
    use protocol::{AutoDownload, AutoDownloadPreset, ErrorCode};
    let mut world = World::signed_in();
    world.connected();

    let Response::AutoDownload { settings } = world.ask(Request::GetAutoDownload { account: account() }) else {
        panic!("expected AutoDownload");
    };
    assert!(settings.roaming.disabled);
    assert_eq!(settings.wifi.photo_size_max, 1_048_576);
    assert_eq!(settings.wifi.video_size_max, 10_485_760);
    assert!(!settings.fetch_full_photos);
    assert!(!settings.private_chats);
    assert!(world.calls().iter().any(|c| c == "account.getAutoDownloadSettings"));

    let mut next = settings;
    next.fetch_full_photos = true;
    next.private_chats = true;
    next.wifi = AutoDownloadPreset { disabled: false, photo_size_max: 2_048, video_size_max: 8_192, file_size_max: 4_096 };
    let Response::AutoDownload { settings: saved } = world.ask(Request::SetAutoDownload { account: account(), settings: next }) else {
        panic!("expected AutoDownload after save");
    };
    assert!(saved.fetch_full_photos && saved.private_chats);
    assert_eq!(saved.wifi.photo_size_max, 2_048);
    assert!(
        world.calls().iter().any(|c| c.contains("account.saveAutoDownloadSettings high photo=2048 video=8192 file=4096 preload=1 bitrate=64")),
        "expected the wifi preset to keep the preload flag, got: {:#?}",
        world.calls()
    );
    assert!(world.calls().iter().any(|c| c.contains("account.saveAutoDownloadSettings low ")));
    assert!(world.calls().iter().any(|c| c.contains("account.saveAutoDownloadSettings medium ")));

    let saves = world.called("account.saveAutoDownloadSettings");
    world.steer("autodownload-unread");
    match world.ask(Request::SetAutoDownload { account: account(), settings: AutoDownload::default() }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("CONNECTION_NOT_INITED"), "{detail}");
        }
        other => panic!("expected an unread preset to refuse the save, got {other:?}"),
    }
    assert_eq!(world.called("account.saveAutoDownloadSettings"), saves, "an unread preset is not overwritten: {:#?}", world.calls());

    world.restart();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.steer("photo");
    world.until("the saved private-chat gate to fetch the photo without opening settings", || {
        (world.called("GetFile photo 6014 y") > 0).then_some(())
    });

    world.steer("autodownload-refuse");
    let refused = world.ask(Request::SetAutoDownload {
        account: account(),
        settings: AutoDownload::default(),
    });
    match refused {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("Telegram Premium"), "{detail}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(world.calls().iter().any(|c| c.contains("account.saveAutoDownloadSettings refused")));
}

#[test]
fn channel_boosts_read_the_level_and_applying_one_can_be_refused() {
    use protocol::ErrorCode;
    let world = World::signed_in();
    world.connected();

    let Response::Boosts { status, .. } = world.ask(Request::GetBoosts { conversation: key("c9001") }) else {
        panic!("expected Boosts");
    };
    assert_eq!(status.level, 3);
    assert_eq!(status.boosts, 12);
    assert_eq!(status.next_level_boosts, Some(20));
    assert!(!status.my_boost);
    assert!(
        world.calls().iter().any(|c| c == "premium.getBoostsStatus c9001"),
        "expected getBoostsStatus on the wire, got: {:#?}",
        world.calls()
    );

    world.steer("boost-refuse");
    match world.ask(Request::ApplyBoost { conversation: key("c9001") }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("Telegram Premium"), "{detail}");
        }
        other => panic!("expected a premium refusal, got {other:?}"),
    }
    assert!(
        world.calls().iter().any(|c| c.contains("premium.applyBoost refused")),
        "expected the refused apply on the wire, got: {:#?}",
        world.calls()
    );

    world.steer("boost-refuse 0");
    let Response::Boosts { status, .. } = world.ask(Request::ApplyBoost { conversation: key("c9001") }) else {
        panic!("expected Boosts after apply");
    };
    assert!(status.my_boost);
    assert_eq!(status.level, 3);
    assert_eq!(status.boosts, 13);
    assert!(
        world.calls().iter().any(|c| c.contains("premium.applyBoost c9001 slot=1")),
        "expected applyBoost on the wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn bot_commands_keyboard_and_start_can_be_refused() {
    use protocol::BotKeyKind;
    let world = World::signed_in();
    world.connected();

    let Response::BotMenu { menu, .. } = world.ask(Request::GetBotMenu { conversation: key("u7004") }) else {
        panic!("expected BotMenu");
    };
    assert_eq!(menu.description, "Trail reports.");
    assert_eq!(menu.menu_label, "Map");
    assert_eq!(menu.menu_url, "https://t.me/trail_bot");
    assert_eq!(
        menu.commands.iter().map(|cmd| cmd.command.as_str()).collect::<Vec<_>>(),
        ["start", "report", "closures"]
    );

    match world.ask(Request::GetBotMenu { conversation: key("u7003") }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("cannot be started"), "{detail}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    let trail = world.arrived("u7004", "4");
    let board = trail.keyboard.expect("reply keyboard");
    assert!(board.resize);
    assert_eq!(board.placeholder.as_deref(), Some("Ask the trail"));
    assert_eq!(board.rows[0][0].text, "Ridge");
    assert_eq!(board.rows[0][0].kind, BotKeyKind::Text);

    world.send("u7004", "1", "Ridge", None);
    world.until("the keyboard text on the wire", || world.calls().iter().any(|call| call.contains("SendMessage u7004 Ridge")).then_some(()));
    assert!(
        world.calls().iter().any(|call| call.contains("SendMessage u7004 Ridge")),
        "expected the keyboard text on the wire, got: {:#?}",
        world.calls()
    );

    assert!(matches!(
        world.ask(Request::StartBot { conversation: key("u7004"), start_param: "ridge".into() }),
        Response::Ok
    ));
    assert!(
        world.calls().iter().any(|call| call.contains("messages.startBot u7004 ridge")),
        "expected startBot on the wire, got: {:#?}",
        world.calls()
    );

    world.steer("start-refuse");
    match world.ask(Request::StartBot { conversation: key("u7004"), start_param: "ridge".into() }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("cannot be started"), "{detail}");
        }
        other => panic!("expected a start refusal, got {other:?}"),
    }
    assert!(world.calls().iter().any(|call| call.contains("messages.startBot refused")));

    match world.ask(Request::SharePhone { conversation: key("u7004") }) {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.to_lowercase().contains("phone"), "{detail}");
        }
        other => panic!("expected a phone refusal, got {other:?}"),
    }
}

#[test]
fn shared_contact_send_and_receive() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.share_contact, "Telegram must advertise share_contact capability");

    let shared = protocol::SharedContact {
        name: "Odalys Brandt".into(),
        phones: vec!["+15550100777".into()],
        emails: vec!["odalys@example.org".into()],
        organization: "Trail Crew".into(),
        avatar: None,
        person_id: None,
    };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: key("u7003"),
            client_id: "contact-tg-1".into(),
            text: String::new(),
            reply_to: None,
            with: protocol::Outgoing { contact: Some(shared), ..Default::default() },
        }),
        Response::Sent { .. }
    ));
    let sent = world.until("contact sent", || {
        world
            .messages("u7003")
            .into_iter()
            .find(|m| !m.contacts.is_empty() && m.state == SendState::Sent)
    });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert_eq!(sent.kind, MessageKind::Normal);
    assert_eq!(sent.contacts.len(), 1);
    let card = &sent.contacts[0];
    assert_eq!(card.name, "Odalys Brandt");
    assert_eq!(card.phones, vec!["+15550100777"]);
    assert_eq!(card.emails, vec!["odalys@example.org"]);
    assert_eq!(card.organization, "Trail Crew");
    assert_eq!(card.person_id.as_deref(), Some("u7002"));
    assert!(
        world.calls().iter().any(|c| c.contains("SendMedia u7003 contact:Odalys:Brandt:+15550100777")),
        "expected InputMediaContact on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn forward_to_chat_saved_messages_and_refuses_noforwards() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.forward, "Telegram must advertise forward capability");

    // Open Odalys (u7002) so we have a message id to forward.
    let msgs = world.until("u7002 messages", || {
        let list = world.messages("u7002");
        (!list.is_empty()).then_some(list)
    });
    let msg_id = msgs[0].id.clone();

    // Forward to another chat (Tobin u7003) and to Saved Messages (u7001 / InputPeer::PeerSelf).
    let resp = world.ask(Request::Forward {
        from: key("u7002"),
        message_ids: vec![msg_id.clone()],
        to: vec![key("u7003"), key("u7001")],
        note: String::new(),
    });
    match resp {
        Response::Forwarded { results } => {
            assert_eq!(results.len(), 2);
            assert!(results[0].ok, "forward to u7003 failed: {:?}", results[0]);
            assert!(results[1].ok, "forward to Saved Messages failed: {:?}", results[1]);
        }
        other => panic!("expected Forwarded, got {other:?}"),
    }
    assert!(
        world.calls().iter().any(|c| c.contains("ForwardMessages u7002 -> u7003")),
        "expected ForwardMessages to u7003, got: {:#?}",
        world.calls()
    );
    assert!(
        world.calls().iter().any(|c| c.contains("ForwardMessages u7002 -> self")),
        "expected ForwardMessages to Saved Messages (InputPeer::PeerSelf), got: {:#?}",
        world.calls()
    );
    // Target chat u7003 should now show the forwarded message with Forwarded metadata.
    let fwd = world.until("forwarded message in u7003", || {
        world.messages("u7003").into_iter().find(|m| m.forwarded.is_some())
    });
    assert!(fwd.forwarded.is_some());

    // Protected noforwards chat refuses forwarding clearly.
    world.steer("noforwards c9001");
    let notes_msgs = world.until("c9001 messages", || {
        let list = world.messages("c9001");
        (!list.is_empty()).then_some(list)
    });
    let blocked = world.ask(Request::Forward {
        from: key("c9001"),
        message_ids: vec![notes_msgs[0].id.clone()],
        to: vec![key("u7003")],
        note: String::new(),
    });
    match blocked {
        Response::Error { detail, .. } => {
            assert!(
                detail.to_lowercase().contains("forward"),
                "expected clear noforwards refusal, got: {detail}"
            );
        }
        Response::Forwarded { results } => {
            assert!(!results[0].ok, "noforwards chat must refuse forwarding");
            assert!(
                results[0].error.as_deref().unwrap_or("").to_lowercase().contains("forward"),
                "expected clear noforwards refusal, got: {:?}",
                results[0]
            );
        }
        other => panic!("expected refusal for noforwards chat, got {other:?}"),
    }
}

#[test]
fn global_search_across_chats_with_pagination() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.search_global, "Telegram must advertise search_global capability");

    // Search across all chats without having opened them first.
    let resp = world.ask(Request::SearchNetwork {
        conversation: None,
        space: None,
        account: Some(account()),
        network: None,
        query: "trail".into(),
        filters: protocol::SearchFilters::default(),
        cursor: None,
        home: false,
    });
    let (hits, not_searched) = match resp {
        Response::MessageHits { hits, not_searched, .. } => (hits, not_searched),
        other => panic!("expected MessageHits, got {other:?}"),
    };
    assert!(not_searched.is_empty(), "Telegram should have been searched: {not_searched:?}");
    let distinct_chats: std::collections::HashSet<String> = hits.iter().map(|h| h.conversation.id.clone()).collect();
    assert!(
        distinct_chats.len() >= 2,
        "expected global search hits from at least 2 distinct chats, got {distinct_chats:?} (hits: {hits:?})"
    );
    assert!(
        world.calls().iter().any(|c| c.starts_with("SearchGlobal trail")),
        "expected messages.searchGlobal on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn report_message_and_peer() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.abuse_reports, "Telegram must advertise abuse_reports capability");

    // Query report reasons.
    match world.ask(Request::ReportReasons {
        target: protocol::ReportTarget::Message { conversation: key("u7005"), message_id: "6".into() },
    }) {
        Response::ReportReasons { reasons } => {
            assert!(!reasons.is_empty());
            assert!(reasons.contains(&protocol::ReportReason::Spam));
        }
        other => panic!("expected ReportReasons, got {other:?}"),
    }

    // Report a message via messages.report (walks Telegram's option tree to Reported).
    let resp = world.ask(Request::Report {
        target: protocol::ReportTarget::Message { conversation: key("u7005"), message_id: "6".into() },
        reason: protocol::ReportReason::Spam,
        note: "Unsolicited prize scam".into(),
        block: false,
    });
    assert!(matches!(resp, Response::Ok), "expected Ok reporting message, got {resp:?}");
    assert!(
        world.calls().iter().any(|c| c.starts_with("messages.report u7005 [6]")),
        "expected messages.report on wire, got: {:#?}",
        world.calls()
    );

    // Report a peer (user/channel) via account.reportPeer with reason mapping.
    let resp = world.ask(Request::Report {
        target: protocol::ReportTarget::Person(key("u7006")),
        reason: protocol::ReportReason::Impersonation,
        note: "Fake support account".into(),
        block: false,
    });
    assert!(matches!(resp, Response::Ok), "expected Ok reporting peer, got {resp:?}");
    assert!(
        world.calls().iter().any(|c| c.contains("account.reportPeer u7006 fake")),
        "expected account.reportPeer with fake reason on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn save_and_unsave_gif() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.save_gif, "Telegram must advertise save_gif capability");

    // Initial saved GIFs contain d6200.
    let initial = match world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: String::new(),
        cursor: None,
    }) {
        Response::Catalog { items, .. } => items,
        other => panic!("expected Catalog, got {other:?}"),
    };
    assert!(initial.iter().any(|i| i.id == "d6200"), "expected d6200 in initial saved GIFs: {initial:?}");

    // Unsave d6200 via Request::SaveGif { unsave: true }.
    let resp = world.ask(Request::SaveGif {
        account: account(),
        id: "d6200".into(),
        unsave: true,
    });
    assert!(matches!(resp, Response::Ok), "expected Ok unsaving GIF, got {resp:?}");
    assert!(
        world.calls().iter().any(|c| c == "SaveGif d6200 unsave=true"),
        "expected messages.saveGif unsave=true on wire, got: {:#?}",
        world.calls()
    );

    // Saved GIFs list should no longer contain d6200.
    let after_unsave = match world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: String::new(),
        cursor: None,
    }) {
        Response::Catalog { items, .. } => items,
        other => panic!("expected Catalog, got {other:?}"),
    };
    assert!(!after_unsave.iter().any(|i| i.id == "d6200"), "d6200 should be removed after unsave: {after_unsave:?}");

    // Re-save d6200 via Request::SaveGif { unsave: false }.
    let resp = world.ask(Request::SaveGif {
        account: account(),
        id: "d6200".into(),
        unsave: false,
    });
    assert!(matches!(resp, Response::Ok), "expected Ok saving GIF, got {resp:?}");
    assert!(
        world.calls().iter().any(|c| c == "SaveGif d6200 unsave=false"),
        "expected messages.saveGif unsave=false on wire, got: {:#?}",
        world.calls()
    );

    // Saved GIFs list should contain d6200 again.
    let after_save = match world.ask(Request::Catalog {
        account: account(),
        kind: protocol::CatalogKind::Gifs,
        space: None,
        query: String::new(),
        cursor: None,
    }) {
        Response::Catalog { items, .. } => items,
        other => panic!("expected Catalog, got {other:?}"),
    };
    assert!(after_save.iter().any(|i| i.id == "d6200"), "d6200 should be back after save: {after_save:?}");
}

#[test]
fn paid_star_reactions_big_reaction_and_default_quick_reaction() {
    let world = World::signed_in();
    world.connected();
    // The dialog list only previews the forum. Opening it lists the topic, and the topic's posts arrive once that row exists.
    let _ = world.messages("c9002");
    world.until("the tools topic", || world.conversation("c9002_31"));
    world.until("workshop tools topic", || world.message("c9002_31", "41"));

    // Read reaction rules for Workshop: paid reactions enabled, big enabled, and default quick reaction from help.getConfig.
    let rules = match world.ask(Request::Reactions { conversation: key("c9002_31") }) {
        Response::Reactions { rules, .. } => rules,
        other => panic!("expected Reactions, got {other:?}"),
    };
    assert!(rules.paid, "Workshop channel should advertise paid Star reactions");
    assert!(rules.big, "Telegram should advertise big reactions");
    assert_eq!(rules.quick.as_deref(), Some("❤"), "default quick reaction read from help.getConfig");

    // A paid ⭐ reaction arrives on message 41 with a top-senders leaderboard.
    world.steer("paid-reaction");
    let with_paid = world.until("paid star reaction", || {
        let msg = world.message("c9002_31", "41")?;
        msg.reactions.iter().any(|r| r.emoji == "⭐" && r.count == 5).then_some(msg)
    });
    let paid_chip = with_paid.reactions.iter().find(|r| r.emoji == "⭐").unwrap();
    assert_eq!(paid_chip.who, vec!["u7002".to_owned()], "top_reactors populates the leaderboard");

    // Guard: clicking ⭐ as an ordinary Request::React MUST be refused and NEVER send ReactionEmoji("⭐").
    let stray = world.ask(Request::React {
        conversation: key("c9002_31"),
        message_id: "41".into(),
        emoji: "⭐".into(),
        on: true,
        burst: false,
    });
    assert!(matches!(stray, Response::Error { code: ErrorCode::Refused, .. }), "expected refusal for stray ⭐ click, got {stray:?}");
    assert!(
        !world.calls().iter().any(|c| c.starts_with("SendReaction") && c.contains("⭐")),
        "must never send plain reactionEmoji ⭐ on wire: {:#?}",
        world.calls()
    );

    // Check Stars balance via payments.getStarsStatus.
    let Response::StarsBalance { balance, .. } = world.ask(Request::StarsBalance { account: account() }) else {
        panic!("expected StarsBalance");
    };
    assert_eq!(balance, 250);

    // Confirmed paid flow sends messages.sendPaidReaction with the right count and privacy.
    assert_eq!(
        world.ask(Request::SendPaidReaction {
            conversation: key("c9002_31"),
            message_id: "41".into(),
            count: 10,
            anonymous: true,
        }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c == "SendPaidReaction c9002 41 count=10 private=anonymous"),
        "expected SendPaidReaction on wire, got: {:#?}",
        world.calls()
    );
    let updated = world.until("paid count incremented", || {
        let msg = world.message("c9002_31", "41")?;
        msg.reactions.iter().find(|r| r.emoji == "⭐" && r.count == 15 && r.mine).cloned()
    });
    assert_eq!(updated.count, 15);

    // Balance reflects spent Stars.
    let Response::StarsBalance { balance: after_bal, .. } = world.ask(Request::StarsBalance { account: account() }) else {
        panic!("expected StarsBalance after spend");
    };
    assert_eq!(after_bal, 240);

    // Toggle paid reaction privacy via messages.togglePaidReactionPrivacy.
    assert_eq!(
        world.ask(Request::TogglePaidReactionPrivacy {
            conversation: key("c9002_31"),
            message_id: "41".into(),
            anonymous: false,
        }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c == "TogglePaidReactionPrivacy c9002 41 private=default"),
        "expected TogglePaidReactionPrivacy on wire, got: {:#?}",
        world.calls()
    );

    // Sending a big reaction (`burst: true`) sets `big: true` on messages.sendReaction.
    assert_eq!(
        world.ask(Request::React {
            conversation: key("u7002"),
            message_id: "12".into(),
            emoji: "🔥".into(),
            on: true,
            burst: true,
        }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c == "SendReaction u7002 12 🔥 big"),
        "expected big=true SendReaction on wire, got: {:#?}",
        world.calls()
    );

    // Changing the default quick reaction calls messages.setDefaultReaction and updates rules.quick.
    assert_eq!(
        world.ask(Request::SetDefaultReaction { account: account(), emoji: "👍".into() }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c == "SetDefaultReaction 👍"),
        "expected SetDefaultReaction on wire, got: {:#?}",
        world.calls()
    );
    let refreshed = match world.ask(Request::Reactions { conversation: key("u7002") }) {
        Response::Reactions { rules, .. } => rules,
        other => panic!("expected refreshed Reactions, got {other:?}"),
    };
    assert_eq!(refreshed.quick.as_deref(), Some("👍"));
}

#[test]
fn auto_delete_timer_chat_and_default() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.disappearing, "Telegram must advertise disappearing capability");

    // Set Odalys chat auto-delete to 1 week (604_800s).
    assert_eq!(
        world.ask(Request::SetDisappearing { conversation: key("u7002"), seconds: 604_800 }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c == "SetHistoryTtl u7002 604800"),
        "expected SetHistoryTtl u7002 604800 on wire, got: {:#?}",
        world.calls()
    );
    // Conversation row & ConversationInfo both report the active timer.
    let row = world.until("conversation disappearing timer", || {
        world.conversation("u7002").filter(|c| c.disappearing == Some(604_800))
    });
    assert_eq!(row.disappearing, Some(604_800));
    match world.ask(Request::ConversationInfo { conversation: key("u7002") }) {
        Response::ConversationInfo { info } => assert_eq!(info.disappearing, Some(604_800)),
        other => panic!("expected ConversationInfo, got {other:?}"),
    }
    // Service message in the chat notes the auto-delete timer.
    let service = world.until("auto-delete service line", || {
        world.messages("u7002").into_iter().find(|m| m.text.contains("auto-delete after 1 week"))
    });
    assert_eq!(service.kind, protocol::MessageKind::System);

    // Turn auto-delete off (0s).
    assert_eq!(
        world.ask(Request::SetDisappearing { conversation: key("u7002"), seconds: 0 }),
        Response::Ok
    );
    world.until("conversation disappearing cleared", || {
        world.conversation("u7002").filter(|c| c.disappearing.is_none())
    });
    match world.ask(Request::ConversationInfo { conversation: key("u7002") }) {
        Response::ConversationInfo { info } => assert_eq!(info.disappearing, None),
        other => panic!("expected ConversationInfo, got {other:?}"),
    }

    // Account-wide default auto-delete timer for new chats.
    assert_eq!(
        world.ask(Request::GetDefaultDisappearing { account: account() }),
        Response::DefaultDisappearing { seconds: 0 }
    );
    assert_eq!(
        world.ask(Request::SetDefaultDisappearing { account: account(), seconds: 86_400 }),
        Response::Ok
    );
    assert_eq!(
        world.ask(Request::GetDefaultDisappearing { account: account() }),
        Response::DefaultDisappearing { seconds: 86_400 }
    );
    assert!(world.calls().iter().any(|c| c == "GetDefaultHistoryTtl"));
    assert!(world.calls().iter().any(|c| c == "SetDefaultHistoryTtl 86400"));
}

#[test]
fn chat_folders_create_edit_add_reorder_suggest_and_delete() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.folders_edit, "Telegram must advertise folders_edit capability");

    // Create a new folder containing Odalys and Trail crew.
    let created = match world.ask(Request::FolderSave {
        account: account(),
        folder: protocol::FolderSpec {
            name: "Hiking".into(),
            include: vec!["u7002".into(), "g8001".into()],
            ..protocol::FolderSpec::default()
        },
    }) {
        Response::Folder { folder } => folder,
        other => panic!("expected Folder, got {other:?}"),
    };
    assert_eq!(created.name, "Hiking");
    let fid = created.key.id.clone();
    assert!(world.calls().iter().any(|c| c == &format!("UpdateDialogFilter {fid} Hiking")));

    // Folders list includes it, and both chats report membership in Conversation::folders.
    let folders = match world.ask(Request::Folders { network: Network::Telegram }) {
        Response::Folders { folders } => folders,
        other => panic!("expected Folders, got {other:?}"),
    };
    assert!(folders.iter().any(|f| f.key.id == fid && f.name == "Hiking"), "{folders:?}");
    let odalys = world.until("Odalys in Hiking folder", || {
        world.conversation("u7002").filter(|c| c.folders.contains(&fid))
    });
    assert!(odalys.folders.contains(&fid));
    let crew = world.until("Crew in Hiking folder", || {
        world.conversation("g8001").filter(|c| c.folders.contains(&fid))
    });
    assert!(crew.folders.contains(&fid));

    // Edit/rename the folder.
    match world.ask(Request::FolderSave {
        account: account(),
        folder: protocol::FolderSpec {
            id: Some(fid.clone()),
            name: "Trails".into(),
            include: vec!["u7002".into(), "g8001".into()],
            ..protocol::FolderSpec::default()
        },
    }) {
        Response::Folder { folder } => assert_eq!((folder.key.id.as_str(), folder.name.as_str()), (fid.as_str(), "Trails")),
        other => panic!("expected renamed Folder, got {other:?}"),
    }

    // Add Tobin to the folder via Request::File, then remove him.
    assert_eq!(
        world.ask(Request::File {
            conversation: key("u7003"),
            pinned: None,
            archived: None,
            folder_add: Some(fid.clone()),
            folder_remove: None,
        }),
        Response::Ok
    );
    world.until("Tobin added to Trails", || {
        world.conversation("u7003").filter(|c| c.folders.contains(&fid))
    });
    assert_eq!(
        world.ask(Request::File {
            conversation: key("u7003"),
            pinned: None,
            archived: None,
            folder_add: None,
            folder_remove: Some(fid.clone()),
        }),
        Response::Ok
    );
    world.until("Tobin removed from Trails", || {
        world.conversation("u7003").filter(|c| !c.folders.contains(&fid))
    });

    // Reorder folders so the new folder is first.
    let order = vec![fid.clone(), "3".into(), "2".into()];
    assert_eq!(
        world.ask(Request::FoldersReorder { account: account(), order: order.clone() }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == &format!("UpdateDialogFiltersOrder {fid},3,2")));
    let reordered = match world.ask(Request::Folders { network: Network::Telegram }) {
        Response::Folders { folders } => folders,
        other => panic!("expected Folders, got {other:?}"),
    };
    assert_eq!(reordered.first().map(|f| f.key.id.as_str()), Some(fid.as_str()), "{reordered:?}");

    // Suggested folders returns Telegram's suggestions.
    match world.ask(Request::SuggestedFolders { account: account() }) {
        Response::SuggestedFolders { folders } => {
            assert!(folders.iter().any(|s| s.spec.name == "Unread"), "{folders:?}");
        }
        other => panic!("expected SuggestedFolders, got {other:?}"),
    }

    // Delete the folder and verify it disappears from Folders and from Odalys's row.
    assert_eq!(
        world.ask(Request::FolderDelete { account: account(), folder_id: fid.clone() }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == &format!("UpdateDialogFilter {fid} delete")));
    world.until("Odalys left deleted folder", || {
        world.conversation("u7002").filter(|c| !c.folders.contains(&fid))
    });

    // Free account folder limit (10 folders) gives a clear message mentioning limit / Premium.
    for i in 0..8 {
        assert!(matches!(
            world.ask(Request::FolderSave {
                account: account(),
                folder: protocol::FolderSpec {
                    name: format!("Extra {i}"),
                    contacts: true,
                    ..protocol::FolderSpec::default()
                },
            }),
            Response::Folder { .. }
        ));
    }
    match world.ask(Request::FolderSave {
        account: account(),
        folder: protocol::FolderSpec {
            name: "Overflow".into(),
            contacts: true,
            ..protocol::FolderSpec::default()
        },
    }) {
        Response::Error { detail, .. } => {
            assert!(detail.contains("10-folder") || detail.contains("Premium"), "{detail}");
        }
        other => panic!("expected limit error, got {other:?}"),
    }
}

#[test]
fn forum_topics_crud_search_icons_and_paging_past_fifty() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_TOPICS", "120")]);
    world.connected();

    // Workshop forum carries manage_topics right.
    let forum = world.conversation("c9002").expect("Workshop forum row");
    assert!(forum.rights.manage_topics, "expected manage_topics on Workshop: {:?}", forum.rights);

    // Paging past 50 topics loads all 120 topics via GetForumTopics cursor paging.
    let p1 = match world.ask(Request::Topics { forum: key("c9002"), offset: 0, limit: 50, landing: false, query: None, sort: protocol::TopicSort::Activity, tag: None }) {
        Response::Conversations { conversations, total, .. } => {
            assert_eq!(total, 120);
            assert_eq!(conversations.len(), 50);
            conversations
        }
        other => panic!("expected page 1 Conversations, got {other:?}"),
    };
    let p2 = match world.ask(Request::Topics { forum: key("c9002"), offset: 50, limit: 50, landing: false, query: None, sort: protocol::TopicSort::Activity, tag: None }) {
        Response::Conversations { conversations, total, .. } => {
            assert_eq!(total, 120);
            assert_eq!(conversations.len(), 50);
            conversations
        }
        other => panic!("expected page 2 Conversations, got {other:?}"),
    };
    let p3 = match world.ask(Request::Topics { forum: key("c9002"), offset: 100, limit: 50, landing: false, query: None, sort: protocol::TopicSort::Activity, tag: None }) {
        Response::Conversations { conversations, total, .. } => {
            assert_eq!(total, 120);
            assert_eq!(conversations.len(), 20);
            conversations
        }
        other => panic!("expected page 3 Conversations, got {other:?}"),
    };
    let unique: std::collections::HashSet<String> = p1
        .into_iter()
        .chain(p2)
        .chain(p3)
        .map(|c| c.key.id)
        .collect();
    assert_eq!(unique.len(), 120, "all 120 forum topics must be reachable past the first 50");

    // Searching topics filters via GetForumTopics `q`.
    match world.ask(Request::Topics {
        forum: key("c9002"),
        offset: 0,
        limit: 20,
        landing: false,
        query: Some("Topic #115".into()),
        sort: protocol::TopicSort::Activity, tag: None,
    }) {
        Response::Conversations { conversations, .. } => {
            assert_eq!(conversations.len(), 1, "{conversations:?}");
            assert_eq!(conversations[0].title, "Topic #115");
        }
        other => panic!("expected searched topics, got {other:?}"),
    }

    // Default topic icons catalog.
    match world.ask(Request::TopicIcons { account: account() }) {
        Response::Catalog { items, .. } => {
            assert!(items.iter().any(|i| i.id == "e6501"), "{items:?}");
        }
        other => panic!("expected TopicIcons Catalog, got {other:?}"),
    }

    // Create a new topic.
    let created = match world.ask(Request::TopicCreate {
        forum: key("c9002"),
        title: "Sharpening".into(),
        icon_color: Some(0x6FB9F0),
        icon_emoji_id: Some("e6501".into()),
    }) {
        Response::Conversation { conversation } => conversation,
        other => panic!("expected created topic Conversation, got {other:?}"),
    };
    assert_eq!(created.title, "Sharpening");
    assert_eq!(created.parent.as_deref(), Some("c9002"));
    assert!(world.calls().iter().any(|c| c == "CreateForumTopic c9002 Sharpening"));

    // Rename, close, and pin the topic.
    assert_eq!(
        world.ask(Request::TopicUpdate {
            topic: created.key.clone(),
            title: Some("Chisels".into()),
            icon_emoji_id: None,
            closed: Some(true),
            hidden: None,
            pinned: Some(true),
        }),
        Response::Ok
    );
    let updated = world.until("topic renamed, closed, and pinned", || {
        world.conversation(&created.key.id).filter(|c| c.title == "Chisels" && c.read_only && c.pin.is_some())
    });
    assert_eq!(updated.title, "Chisels");
    assert!(updated.read_only);
    assert!(updated.pin.is_some());

    // Delete the topic.
    assert_eq!(world.ask(Request::TopicDelete { topic: created.key.clone() }), Response::Ok);
    world.until("deleted topic gone", || world.conversation(&created.key.id).is_none().then_some(()));
}

#[test]
fn a_chat_is_pinned_unpinned_archived_and_unarchived_on_telegram() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.pin_chats > 0 && can.archive, "the row menu offers both only where the account has them");
    let file = |id: &str, pinned: Option<bool>, archived: Option<bool>| {
        world.ask(Request::File { conversation: key(id), pinned, archived, folder_add: None, folder_remove: None })
    };
    let main = || world.home(None, Shelf::Main).into_iter().map(|row| row.key.id).collect::<Vec<_>>();
    world.until("the group in the main list", || main().contains(&"g8001".to_owned()).then_some(()));
    assert_eq!(world.conversation("g8001").unwrap().pin, None);

    // Pinned: one request, and the row goes above the pin that was there, at once.
    assert_eq!(file("g8001", Some(true), None), Response::Ok);
    assert_eq!(world.called("ToggleDialogPin g8001 true"), 1, "{:?}", world.calls());
    world.until("the group pinned first", || (world.conversation("g8001")?.pin == Some(0)).then_some(()));
    assert_eq!(world.conversation("u7002").unwrap().pin, Some(1));
    assert_eq!(main()[..2], ["g8001", "u7002"]);

    // Unpinned: Odalys is the first pin again.
    assert_eq!(file("g8001", Some(false), None), Response::Ok);
    assert_eq!(world.called("ToggleDialogPin g8001 false"), 1);
    world.until("the group unpinned", || world.conversation("g8001")?.pin.is_none().then_some(()));
    assert_eq!(world.conversation("u7002").unwrap().pin, Some(0));

    // Archived: folder 1, and it leaves the main list for the archive's shelf.
    assert_eq!(file("g8001", None, Some(true)), Response::Ok);
    assert_eq!(world.called("EditPeerFolders g8001 1"), 1);
    world.until("the group archived", || world.conversation("g8001")?.archived.then_some(()));
    assert!(!main().contains(&"g8001".to_owned()));
    assert!(world.home(None, Shelf::Archive).iter().any(|row| row.key.id == "g8001"));

    // Unarchived: folder 0, back in the main list.
    assert_eq!(file("g8001", None, Some(false)), Response::Ok);
    assert_eq!(world.called("EditPeerFolders g8001 0"), 1);
    world.until("the group back in the main list", || main().contains(&"g8001".to_owned()).then_some(()));

    // Telegram's pin limit is said in words, and the row stays unpinned.
    world.steer("pins-full");
    match file("g8001", Some(true), None) {
        Response::Error { detail, .. } => assert_eq!(detail, "You've pinned as many chats as Telegram allows. Unpin one first."),
        other => panic!("{other:?}"),
    }
    assert_eq!(world.conversation("g8001").unwrap().pin, None);
}

#[test]
fn message_effects_are_listed_sent_and_shown_by_their_emoji() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.effects, "the composer offers effects only where the account has them");
    world.arrived("u7002", "12");

    // The effects Telegram offers: the Premium one is locked for this account.
    let effects = || match world.ask(Request::MessageEffects { account: account() }) {
        Response::Catalog { items, .. } => items.into_iter().map(|item| (item.id, item.name, item.locked)).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let party = "5046509860389126442".to_owned();
    assert_eq!(effects(), [(party.clone(), "\u{1F389}".to_owned(), false), ("5104841245755180586".to_owned(), "\u{1F525}".to_owned(), true)]);

    // Sent with the effect: the request names it, and the message that comes back shows its emoji.
    let asked = Request::Send {
        conversation: key("u7002"),
        client_id: "fx-1".into(),
        text: "Congratulations".into(),
        reply_to: None,
        with: protocol::Outgoing { effect: Some(party.clone()), ..protocol::Outgoing::default() },
    };
    assert!(matches!(world.ask(asked), Response::Sent { .. }));
    world.until("the send to name the effect", || (world.called(&format!("SendMessage u7002 Congratulations [effect={party}]")) == 1).then_some(()));
    let sent = world.until("the sent message", || world.messages("u7002").into_iter().find(|message| message.text == "Congratulations" && message.state == SendState::Sent));
    assert_eq!(sent.effect.as_deref(), Some("\u{1F389}"));

    // One that arrives with an effect is shown the same way, and one without has none.
    world.steer("effect");
    assert_eq!(world.arrived("u7002", "71").effect.as_deref(), Some("\u{1F389}"));
    assert_eq!(world.message("u7002", "12").unwrap().effect, None);

    // Telegram was asked for its effects once on this connection, however often they were used.
    assert_eq!(effects().len(), 2);
    world.steer("effects-asked");
    assert_eq!(world.called("effects asked 1"), 1, "{:?}", world.calls());

    // Telegram plays effects in private chats only, and says so in words.
    world.arrived("g8001", "20");
    let refused = Request::Send {
        conversation: key("g8001"),
        client_id: "fx-2".into(),
        text: "Party".into(),
        reply_to: None,
        with: protocol::Outgoing { effect: Some(party), ..protocol::Outgoing::default() },
    };
    world.ask(refused);
    let failed = world.until("the refusal on the message", || {
        world.messages("g8001").into_iter().find(|message| message.text == "Party" && matches!(message.state, SendState::Failed { .. }))
    });
    assert!(matches!(&failed.state, SendState::Failed { reason } if reason.contains("private chats only")), "{:?}", failed.state);
}

#[test]
fn a_call_discard_with_need_rating_prompts_and_sends_set_call_rating() {
    let world = World::signed_in();
    world.connected();
    world.arrived("u7002", "12");

    // A discard with need_rating=false does not prompt.
    let since = world.status().1;
    world.steer("call-discard-no-rate");

    // A discard with need_rating=true and need_debug=true prompts for a rating (never sends SaveCallDebug)
    // and stores the finished call row with its call_id so the user can also rate from the row.
    world.steer("call-rate");
    let mut cursor = since;
    let prompt = world.until("Change::CallRating prompt", || {
        match world.ask(Request::Wait { since: cursor }) {
            Response::Changes { seq, changes, .. } => {
                cursor = seq;
                changes.into_iter().find_map(|change| match change {
                    protocol::Change::CallRating { prompt } => Some(prompt),
                    _ => None,
                })
            }
            other => panic!("unexpected Wait response: {other:?}"),
        }
    });
    assert_eq!(prompt.conversation, key("u7002"));
    assert_eq!(prompt.call_id, "90001");
    assert_eq!(prompt.seconds, Some(95));
    assert!(!prompt.video);

    // Wait for the call service row in Odalys's chat and confirm it carries call_id.
    let row = world.arrived("u7002", "72");
    assert_eq!(row.call.as_ref().and_then(|c| c.call_id.as_deref()), Some("90001"));
    assert_eq!(row.call.as_ref().map(|c| c.outcome), Some(protocol::CallOutcome::Ended));

    // Tinyclient does not upload non-tgcalls debug logs even when need_debug is true.
    assert_eq!(world.called("SaveCallDebug"), 0, "{:?}", world.calls());

    // Submitting a 3-star rating from the prompt sends phone.setCallRating with user_initiative=false
    // and includes the selected issue hashtags plus optional comment.
    let rated = world.ask(Request::CallRate {
        account: account(),
        call_id: "90001".into(),
        rating: 3,
        reasons: vec!["Echo".into(), "Noise".into()],
        comment: "Audio cut out".into(),
        user_initiative: false,
    });
    assert!(matches!(rated, Response::Ok), "expected Ok, got {rated:?}");
    assert_eq!(
        world.called("SetCallRating 90001 stars=3 user=false comment=#echo #noise\\nAudio cut out"),
        1,
        "{:?}",
        world.calls()
    );

    // Rating again from the finished call row sends user_initiative=true; 5 stars omits reason tags.
    let from_row = world.ask(Request::CallRate {
        account: account(),
        call_id: "90001".into(),
        rating: 5,
        reasons: vec!["Echo".into()],
        comment: "Great".into(),
        user_initiative: true,
    });
    assert!(matches!(from_row, Response::Ok), "expected Ok, got {from_row:?}");
    assert_eq!(
        world.called("SetCallRating 90001 stars=5 user=true comment=Great"),
        1,
        "{:?}",
        world.calls()
    );

    // Out-of-range rating and unknown call id are refused with clear words.
    assert!(matches!(
        world.ask(Request::CallRate {
            account: account(),
            call_id: "90001".into(),
            rating: 0,
            reasons: Vec::new(),
            comment: String::new(),
            user_initiative: false,
        }),
        Response::Error { code: protocol::ErrorCode::Refused, detail } if detail.contains("one and five stars")
    ));
    assert!(matches!(
        world.ask(Request::CallRate {
            account: account(),
            call_id: "55555".into(),
            rating: 4,
            reasons: Vec::new(),
            comment: String::new(),
            user_initiative: true,
        }),
        Response::Error { code: protocol::ErrorCode::Refused, detail } if detail.contains("this session")
    ));
}

#[test]
fn a_fifty_megabyte_video_streams_first_bytes_moov_and_alt_quality_without_downloading_the_whole_file() {
    let world = World::signed_in();
    world.connected();
    world.watch(Watching { conversation: Some(key("u7002")), ..Watching::default() });
    world.arrived("u7002", "12");

    world.steer("video-stream");
    let fast = world.arrived("u7002", "73");
    let slow = world.arrived("u7002", "74");
    let fast_att = &fast.attachments[0];
    assert_eq!(fast_att.file.id, "d9500");
    assert_eq!(fast_att.file.bytes, 50 * 1024 * 1024);
    assert_eq!(fast_att.variants.len(), 1);
    assert_eq!(fast_att.variants[0].id, "d9501");
    assert_eq!(fast_att.variants[0].height, 480);
    assert_eq!(slow.attachments[0].file.id, "d9502");

    // Nothing has been downloaded yet for any of the 50 MB videos.
    assert_eq!(world.called("GetFile document 9500"), 0);
    assert_eq!(world.called("GetFile document 9501"), 0);
    assert_eq!(world.called("GetFile document 9502"), 0);

    let mut cursor = world.status().1;
    let mut wait_progress = |wanted_id: &str, min_loaded: u64| -> (String, u64, u64, bool) {
        world.until(&format!("MediaProgress for {wanted_id} >= {min_loaded}"), || {
            match world.ask(Request::Wait { since: cursor }) {
                Response::Changes { seq, changes, .. } => {
                    cursor = seq;
                    changes.into_iter().find_map(|change| match change {
                        protocol::Change::MediaProgress { id, path, loaded, total, complete, .. }
                            if id == wanted_id && loaded >= min_loaded =>
                        {
                            Some((path, loaded, total, complete))
                        }
                        _ => None,
                    })
                }
                other => panic!("unexpected Wait response: {other:?}"),
            }
        })
    };

    // 1. Ask for the first 256 KB window of the 50 MB fast-start video: player gets its first
    //    bytes after ONE piece (not after all 50 MB!).
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec!["d9500".into()],
        size: None,
        retry: false,
        span: Some((0, 256 * 1024)),
    });
    let (path, loaded, total, complete) = wait_progress("d9500", 256 * 1024);
    assert!(!complete);
    assert_eq!(loaded, 256 * 1024);
    assert_eq!(total, 50 * 1024 * 1024);
    assert_eq!(world.called("GetFile document 9500"), 1, "only the first piece should be fetched: {:?}", world.calls());
    let head = std::fs::read(&path).expect("sparse stream file exists");
    assert_eq!(&head[..4], b"FTYP");

    // 2. Scrub to 25 MB: fetches only that 4 KB-aligned piece into the same sparse cache file.
    let scrub_off = 25 * 1024 * 1024u64;
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec!["d9500".into()],
        size: None,
        retry: false,
        span: Some((scrub_off, scrub_off + 256 * 1024)),
    });
    let (path2, loaded2, _, complete2) = wait_progress("d9500", 512 * 1024);
    assert!(!complete2);
    assert_eq!(path2, path);
    assert_eq!(loaded2, 512 * 1024);
    assert_eq!(world.called("GetFile document 9500 at 26214400"), 1);
    assert_eq!(world.called("GetFile document 9500"), 2);
    let mut f = std::fs::File::open(&path2).unwrap();
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(scrub_off)).unwrap();
    let mut mark = [0u8; 4];
    f.read_exact(&mut mark).unwrap();
    assert_eq!(&mark, b"SCRB");

    // 3. Non-fast-start 50 MB video (`supports_streaming = false`): fetches the tail (`moov`) + head
    //    in just 2 pieces, writing `MOOV` at the end and `FTYP` at offset 0.
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec!["d9502".into()],
        size: None,
        retry: false,
        span: Some((0, 256 * 1024)),
    });
    let (slow_path, slow_loaded, slow_total, slow_complete) = wait_progress("d9502", 512 * 1024);
    assert!(!slow_complete);
    assert_eq!(slow_loaded, 512 * 1024);
    assert_eq!(slow_total, 50 * 1024 * 1024);
    assert_eq!(world.called("GetFile document 9502"), 2, "only moov tail + head: {:?}", world.calls());
    let mut sf = std::fs::File::open(&slow_path).unwrap();
    let mut head_mark = [0u8; 4];
    sf.read_exact(&mut head_mark).unwrap();
    assert_eq!(&head_mark, b"FTYP");
    sf.seek(SeekFrom::Start(slow_total - 4)).unwrap();
    let mut tail_mark = [0u8; 4];
    sf.read_exact(&mut tail_mark).unwrap();
    assert_eq!(&tail_mark, b"MOOV");

    // 4. Alternative quality (`alt_documents` 480p variant `d9501`) streams independently on demand.
    let _ = world.ask(Request::Fetch {
        account: account(),
        ids: vec!["d9501".into()],
        size: None,
        retry: false,
        span: Some((0, 256 * 1024)),
    });
    let (_, alt_loaded, alt_total, alt_complete) = wait_progress("d9501", 256 * 1024);
    assert!(!alt_complete);
    assert_eq!(alt_loaded, 256 * 1024);
    assert_eq!(alt_total, 12 * 1024 * 1024);
    assert_eq!(world.called("GetFile document 9501 at 0"), 1);
}

#[test]
fn who_reacted_and_who_read_a_message_are_asked_of_telegram_only_on_demand() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.reactors && can.read_by);
    world.arrived("g8001", "20");
    world.arrived("u7002", "12");
    // Nothing is asked until a list is opened.
    assert_eq!(world.called("GetMessageReactionsList") + world.called("GetMessageReadParticipants") + world.called("GetOutboxReadDate"), 0);

    // The people behind one reaction, by name.
    let reactors = |conversation: &str, id: &str| {
        match world.ask(Request::Reactors { conversation: key(conversation), message_id: id.into(), emoji: "\u{1F44D}".into(), burst: false, after: None, limit: 100 }) {
            Response::Reactors { reactors, more, .. } => (reactors.into_iter().map(|reactor| (reactor.key.id, reactor.name)).collect::<Vec<_>>(), more),
            other => panic!("{other:?}"),
        }
    };
    let (who, more) = reactors("g8001", "20");
    assert_eq!(who, [("u7002".to_owned(), "Odalys Brandt".to_owned()), ("u7003".to_owned(), "Tobin Reyes".to_owned())]);
    assert!(!more);
    assert_eq!(world.called("GetMessageReactionsList g8001 20 \u{1F44D} offset=None limit=100"), 1, "{:?}", world.calls());
    // A broadcast channel only counts: nobody is listed, and Telegram is not asked.
    world.until("the channel's page", || world.message("c9001", "5"));
    assert_eq!(reactors("c9001", "5"), (Vec::new(), false));
    assert_eq!(world.called("GetMessageReactionsList c9001"), 0);

    // Who read a message in the group, and when the other side of a private chat read one.
    let read_by = |conversation: &str, id: &str| world.ask(Request::ReadBy { conversation: key(conversation), message_id: id.into() });
    let Response::ReadBy { readers } = read_by("g8001", "20") else { panic!("no readers") };
    assert_eq!(readers.iter().map(|reader| (reader.person.id.as_str(), reader.name.as_str(), reader.at)).collect::<Vec<_>>(), [("u7002", "Odalys Brandt", t0() + 100)]);
    assert_eq!(world.called("GetMessageReadParticipants g8001 20"), 1);
    let Response::ReadBy { readers } = read_by("u7002", "12") else { panic!("no read time") };
    assert_eq!((readers.len(), readers[0].at), (1, t0() + 100));
    // Telegram's refusal is said in words: read times are shared both ways or not at all.
    match read_by("u7003", "1") {
        Response::Error { detail, .. } => assert!(detail.starts_with("Telegram shows read times only if you share your own."), "{detail}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn copy_link_is_filled_for_channels_and_forum_topics_and_not_for_private_chats() {
    let world = World::signed_in();
    world.connected();
    // A public channel's post is addressed by the channel's name.
    world.watch(Watching { conversation: Some(key("c9001")), ..Watching::default() });
    world.messages("c9001");
    assert_eq!(world.arrived("c9001", "5").permalink, "https://t.me/fieldnotes/5");
    // A private forum's message carries its topic, and only members can open it.
    world.watch(Watching { conversation: Some(key("c9002")), ..Watching::default() });
    world.messages("c9002");
    world.until("the topic row", || world.conversation("c9002_31"));
    world.watch(Watching { conversation: Some(key("c9002_31")), ..Watching::default() });
    assert_eq!(world.arrived("c9002_31", "41").permalink, "https://t.me/c/9002/31/41");
    // A private chat and a basic group have no address anyone else could open.
    assert_eq!(world.arrived("u7002", "12").permalink, "");
    assert_eq!(world.arrived("g8001", "20").permalink, "");
}

#[test]
fn a_message_is_pinned_unpinned_and_follows_the_pins_made_elsewhere() {
    let world = World::signed_in();
    world.connected();
    // The group's history is here before anything is pinned in it.
    assert!(!world.arrived("g8001", "20").pinned);
    let pins = || match world.ask(Request::Pinned { conversation: key("g8001") }) {
        Response::Pinned { messages, .. } => messages.into_iter().map(|message| message.id).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let pinned = |on: bool| world.until(&format!("message 20 pinned={on}"), || world.message("g8001", "20").filter(|message| message.pinned == on).map(drop));
    let pin = |id: &str, on: bool| world.ask(Request::Pin { conversation: key("g8001"), message_id: id.into(), pinned: on, notify: false, me_only: false });

    // Pinned from here: one request, silent, and the message says so.
    assert_eq!(pin("20", true), Response::Ok);
    assert_eq!(world.called("UpdatePinnedMessage g8001 20 unpin=false silent=true oneside=false"), 1, "{:?}", world.calls());
    pinned(true);
    assert_eq!(pins(), ["20"]);

    // Unpinned and pinned again on another device: the stored message follows, with no request.
    let asked = world.called("UpdatePinnedMessage");
    world.steer("message-unpin 20");
    pinned(false);
    world.steer("message-pin 20");
    pinned(true);
    assert_eq!(world.called("UpdatePinnedMessage"), asked);

    // Unpin all is one request for the whole chat.
    assert_eq!(world.ask(Request::UnpinAll { conversation: key("g8001") }), Response::Ok);
    assert_eq!(world.called("UnpinAllMessages g8001 topic=None"), 1);
    pinned(false);
    assert!(pins().is_empty());

    // A refusal is said in Telegram's words, not as a dropped link.
    match pin("999999", true) {
        Response::Error { detail, .. } => assert_eq!(detail, "That message is no longer there."),
        other => panic!("{other:?}"),
    }
}

#[test]
fn scheduled_messages_list_reschedule_send_now_cancel_and_when_online() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.schedule, "Telegram must advertise schedule capability");

    let future = 1_900_000_000i64;
    // A scheduled send is still a send: the row is pending until Telegram names the placeholder.
    let scheduled = world.ask(Request::Send {
        conversation: key("u7002"),
        client_id: "sched-1".into(),
        text: "Morning check-in".into(),
        reply_to: None,
        with: protocol::Outgoing { schedule: Some(future), ..protocol::Outgoing::default() },
    });
    assert!(matches!(scheduled, Response::Sent { .. }), "expected Sent, got {scheduled:?}");
    world.until("first scheduled placeholder", || {
        world.messages("u7002").into_iter().find(|m| m.text == "Morning check-in")
    });

    // Schedule a "when online" message to Odalys (last seen visible).
    let when_online = world.ask(Request::Send {
        conversation: key("u7002"),
        client_id: "sched-2".into(),
        text: "Ping when you are online".into(),
        reply_to: None,
        with: protocol::Outgoing {
            schedule: Some(protocol::SCHEDULE_WHEN_ONLINE),
            ..protocol::Outgoing::default()
        },
    });
    assert!(matches!(when_online, Response::Sent { .. }), "expected Sent, got {when_online:?}");
    world.until("second scheduled placeholder", || {
        world.messages("u7002").into_iter().find(|m| m.text == "Ping when you are online")
    });

    // "Send when online" to Tobin (hidden last seen) fails with a clear message.
    let hidden = world.ask(Request::Send {
        conversation: key("u7003"),
        client_id: "sched-bad".into(),
        text: "Hidden last seen".into(),
        reply_to: None,
        with: protocol::Outgoing {
            schedule: Some(protocol::SCHEDULE_WHEN_ONLINE),
            ..protocol::Outgoing::default()
        },
    });
    assert!(matches!(hidden, Response::Sent { .. }), "expected Sent, got {hidden:?}");
    let failed = world.until("when-online refused for hidden last seen", || {
        world
            .messages("u7003")
            .into_iter()
            .find(|m| m.text == "Hidden last seen" && matches!(m.state, SendState::Failed { .. }))
    });
    match failed.state {
        SendState::Failed { reason } => assert!(reason.contains("last seen"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }

    // List scheduled messages for Odalys: both pending messages appear.
    let listed = match world.ask(Request::ScheduledMessages { conversation: key("u7002") }) {
        Response::ScheduledMessages { messages } => messages,
        other => panic!("expected ScheduledMessages, got {other:?}"),
    };
    assert_eq!(listed.len(), 2, "{listed:?}");
    let timed = listed.iter().find(|m| m.text == "Morning check-in").expect("timed message");
    let online = listed.iter().find(|m| m.text == "Ping when you are online").expect("when-online message");
    assert_eq!(online.post_at, protocol::SCHEDULE_WHEN_ONLINE);

    // Reschedule & edit the timed message.
    let new_time = future + 3_600;
    match world.ask(Request::RescheduleMessage {
        conversation: key("u7002"),
        scheduled_id: timed.id.clone(),
        text: Some("Updated check-in".into()),
        post_at: new_time,
    }) {
        Response::ScheduledMessage { message } => {
            assert_eq!(message.text, "Updated check-in");
            assert_eq!(message.post_at, new_time);
        }
        other => panic!("expected ScheduledMessage, got {other:?}"),
    }

    // Send the rescheduled message immediately: it moves to real history and leaves scheduled queue.
    assert_eq!(
        world.ask(Request::SendScheduledNow { conversation: key("u7002"), scheduled_id: timed.id.clone() }),
        Response::Ok
    );
    world.until("sent scheduled message in history", || {
        world
            .messages("u7002")
            .into_iter()
            .find(|m| m.id == timed.id && m.text == "Updated check-in")
    });
    assert!(world.calls().iter().any(|c| c.starts_with("SendScheduledMessages u7002")));

    // Cancel the remaining scheduled message.
    assert_eq!(
        world.ask(Request::CancelScheduled { conversation: key("u7002"), scheduled_id: online.id.clone() }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c.starts_with("DeleteScheduledMessages u7002")));
    match world.ask(Request::ScheduledMessages { conversation: key("u7002") }) {
        Response::ScheduledMessages { messages } => assert!(messages.is_empty(), "{messages:?}"),
        other => panic!("expected empty ScheduledMessages, got {other:?}"),
    }
}

#[test]
fn chat_search_filters_paging_hashtags_jump_to_date_calendar_and_shared_tabs() {
    let world = World::with(Some(SESSION), "30", &[("TINYCLIENT_TELEGRAM_SEARCH_BULK", "125")]);
    world.connected();

    // Hashtag search across 125 messages pages past 100 using cursor and reports total.
    let (page1, cursor1, total1) = match world.ask(Request::SearchNetwork {
        conversation: Some(key("g8001")),
        space: None,
        account: None,
        network: None,
        query: "#ridge".into(),
        filters: protocol::SearchFilters::default(),
        cursor: None,
        home: false,
    }) {
        Response::MessageHits { hits, cursor, total, .. } => (hits, cursor, total),
        other => panic!("expected MessageHits, got {other:?}"),
    };
    assert_eq!(total1, Some(125));
    assert_eq!(page1.len(), 100);
    assert!(cursor1.is_some(), "expected continuation cursor past 100 hits");

    let (page2, cursor2) = match world.ask(Request::SearchNetwork {
        conversation: Some(key("g8001")),
        space: None,
        account: None,
        network: None,
        query: "#ridge".into(),
        filters: protocol::SearchFilters::default(),
        cursor: cursor1,
        home: false,
    }) {
        Response::MessageHits { hits, cursor, .. } => (hits, cursor),
        other => panic!("expected page 2 MessageHits, got {other:?}"),
    };
    assert_eq!(page2.len(), 25);
    assert!(cursor2.is_none());

    // Filter by sender (Tobin u7003) + date range (last 10 days).
    let base_t0 = 1_791_892_800i64;
    let after = base_t0 - 10 * 86_400;
    let before = base_t0;
    let filtered = match world.ask(Request::SearchNetwork {
        conversation: Some(key("g8001")),
        space: None,
        account: None,
        network: None,
        query: "#ridge".into(),
        filters: protocol::SearchFilters {
            from: Some(key("u7003")),
            after: Some(after),
            before: Some(before),
            ..protocol::SearchFilters::default()
        },
        cursor: None,
        home: false,
    }) {
        Response::MessageHits { hits, .. } => hits,
        other => panic!("expected filtered MessageHits, got {other:?}"),
    };
    assert!(!filtered.is_empty() && filtered.len() <= 10, "got {} hits", filtered.len());
    assert!(filtered.iter().all(|h| h.sender.contains("Tobin") && h.sent >= after && h.sent <= before), "{filtered:?}");

    // Jump to date resolves the message closest to a target timestamp.
    let target_date = base_t0 - 50 * 86_400;
    match world.ask(Request::JumpToDate { conversation: key("g8001"), date: target_date }) {
        Response::Resolved { conversation, message_id, title } => {
            assert_eq!(conversation, Some(key("g8001")));
            assert!(message_id.is_some(), "expected a resolved message id");
            assert!(title.is_empty());
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
    assert!(world.calls().iter().any(|c| c.contains("GetHistory g8001 date=")));

    // Search calendar lists day buckets with counts.
    match world.ask(Request::SearchCalendar { conversation: key("g8001"), kind: None, offset_date: 0 }) {
        Response::SearchCalendar { days } => {
            assert!(days.len() >= 100, "expected >= 100 calendar days, got {}", days.len());
            assert!(days.iter().all(|d| d.count >= 1));
        }
        other => panic!("expected SearchCalendar, got {other:?}"),
    }

    // Extra Telegram shared tabs: Music, Voice, GIFs.
    for kind in [protocol::SharedKind::Music, protocol::SharedKind::Voice, protocol::SharedKind::Gifs] {
        match world.ask(Request::Shared {
            conversation: key("u7002"),
            kind,
            before: None,
            limit: 20,
            search: true,
        }) {
            Response::Shared { items, total, .. } => {
                assert_eq!(total, Some(1), "expected total=1 for {kind:?}");
                assert_eq!(items.len(), 1, "expected 1 item for {kind:?}: {items:?}");
            }
            other => panic!("expected Shared for {kind:?}, got {other:?}"),
        }
    }
}

#[test]
fn delete_modes_admin_batch_participant_history_and_report_spam() {
    let world = World::with(
        Some(SESSION),
        "30",
        &[("TINYCLIENT_TELEGRAM_ADMIN", "1"), ("TINYCLIENT_TELEGRAM_OLD_GROUP", "1")],
    );
    world.connected();
    world.until("odalys history", || world.message("u7002", "11"));

    // Private 1:1 chat allows deleting the other person's messages.
    let odalys = world.conversation("u7002").expect("Odalys row");
    assert!(odalys.rights.delete_others, "private chat must set delete_others: {:?}", odalys.rights);

    // Delete for me only sends revoke=false.
    assert_eq!(
        world.ask(Request::Delete {
            conversation: key("u7002"),
            message_id: "11".into(),
            scope: protocol::DeleteScope::Me,
        }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == "DeleteMessages [11] revoke false"));
    world.until("message 11 deleted locally", || world.message("u7002", "11").is_none().then_some(()));

    // Batch DeleteMany sends a single DeleteMessages RPC with both IDs (including peer's messages).
    assert_eq!(
        world.ask(Request::DeleteMany {
            conversation: key("u7002"),
            message_ids: vec!["10".into(), "12".into()],
            scope: protocol::DeleteScope::Everyone,
        }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == "DeleteMessages [10, 12] revoke true"));
    world.until("batch deleted locally", || {
        (world.message("u7002", "10").is_none() && world.message("u7002", "12").is_none()).then_some(())
    });

    // Basic group past 48h revoke returns a clear error mentioning 48-hour limit.
    match world.ask(Request::Delete {
        conversation: key("g8001"),
        message_id: "20".into(),
        scope: protocol::DeleteScope::Everyone,
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("48-hour"), "{detail}"),
        other => panic!("expected 48-hour error, got {other:?}"),
    }

    // Supergroup/forum with admin rights advertises delete_others, refuses DeleteScope::Me,
    // and deletes another member's message via channels.deleteMessages.
    // The topic's posts are not on the forum row. Open it so message 41 is stored before
    // the delete is asked: a missing row is "no such message", and the channel rule never runs.
    let workshop = world.conversation("c9002").expect("Workshop row");
    assert!(workshop.rights.delete_others, "admin forum must set delete_others: {:?}", workshop.rights);
    world.messages("c9002");
    world.until("the tools topic", || world.conversation("c9002_31"));
    world.until("the topic's history", || world.message("c9002_31", "41"));
    match world.ask(Request::Delete {
        conversation: key("c9002_31"),
        message_id: "41".into(),
        scope: protocol::DeleteScope::Me,
    }) {
        Response::Error { detail, .. } => assert!(detail.contains("everyone"), "{detail}"),
        other => panic!("expected channel delete-for-everyone error, got {other:?}"),
    }
    assert_eq!(
        world.ask(Request::Delete {
            conversation: key("c9002_31"),
            message_id: "41".into(),
            scope: protocol::DeleteScope::Everyone,
        }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == "channels.DeleteMessages c9002 [41]"));

    // Report spam and delete all messages from a participant in a supergroup.
    assert_eq!(
        world.ask(Request::ReportSpamMessages {
            conversation: key("c9002"),
            person: key("u7003"),
            message_ids: vec!["55".into()],
        }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == "channels.ReportSpam c9002 u7003 [55]"));
    assert_eq!(
        world.ask(Request::DeleteParticipantHistory {
            conversation: key("c9002"),
            person: key("u7003"),
        }),
        Response::Ok
    );
    assert!(world.calls().iter().any(|c| c == "channels.DeleteParticipantHistory c9002 u7003"));
}

#[test]
fn bot_inline_buttons_render_and_fire_callback() {
    let world = World::signed_in();
    world.connected();
    let bot_chat = key("u7004");
    world.steer("bot-buttons");

    let msg = world.until("bot message with inline buttons", || {
        match world.ask(Request::Messages { conversation: bot_chat.clone(), page: Page::Newest, limit: 20 }) {
            Response::Messages { messages, .. } => messages.into_iter().find(|m| m.id == "90"),
            _ => None,
        }
    });
    assert_eq!(msg.buttons.len(), 2, "expected 2 rows of inline buttons: {:?}", msg.buttons);
    assert_eq!(msg.buttons[0].len(), 2);
    assert_eq!(msg.buttons[0][0].label, "Approve");
    let callback_data = match &msg.buttons[0][0].kind {
        protocol::InlineButtonKind::Callback { data, requires_password } => {
            assert!(!requires_password);
            data.clone()
        }
        other => panic!("expected Callback button, got {other:?}"),
    };
    assert_eq!(
        msg.buttons[0][1],
        protocol::InlineButton {
            label: "Docs".into(),
            kind: protocol::InlineButtonKind::Url { url: "https://example.com/docs".into() },
        }
    );
    assert_eq!(
        msg.buttons[1][0],
        protocol::InlineButton {
            label: "Copy code".into(),
            kind: protocol::InlineButtonKind::Copy { text: "ABC-123".into() },
        }
    );
    assert_eq!(
        msg.buttons[1][1],
        protocol::InlineButton {
            label: "Search".into(),
            kind: protocol::InlineButtonKind::SwitchInline { query: "trails ".into(), same_peer: true },
        }
    );

    let resp = world.ask(Request::PressButton {
        conversation: bot_chat,
        message_id: "90".into(),
        data: callback_data,
    });
    match resp {
        Response::ButtonAnswer { answer } => {
            assert_eq!(answer.message.as_deref(), Some("Approved!"));
            assert!(!answer.alert);
        }
        other => panic!("expected ButtonAnswer, got {other:?}"),
    }
    assert!(
        world.calls().iter().any(|c| c == "GetBotCallbackAnswer u7004 90 approve:42"),
        "expected GetBotCallbackAnswer on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn notify_defaults_read_update_exceptions_and_reset() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.notify_defaults, "Telegram must advertise notify_defaults capability");

    // Initial defaults: none muted, with Field Notes channel as an exception.
    let initial = match world.ask(Request::GetNotifyDefaults { account: account() }) {
        Response::NotifyDefaults { defaults } => defaults,
        other => panic!("expected NotifyDefaults, got {other:?}"),
    };
    assert!(!initial.users_muted);
    assert!(!initial.groups_muted);
    assert!(!initial.channels_muted);
    assert!(
        initial.exceptions.iter().any(|e| e.conversation == key("c9001") && e.muted),
        "expected c9001 muted exception, got {:?}",
        initial.exceptions
    );

    // Mute all groups via SetNotifyDefault.
    let after_groups = match world.ask(Request::SetNotifyDefault {
        account: account(),
        scope: protocol::NotifyDefaultScope::Groups,
        muted: true,
    }) {
        Response::NotifyDefaults { defaults } => defaults,
        other => panic!("expected NotifyDefaults, got {other:?}"),
    };
    assert!(after_groups.groups_muted);
    assert!(
        world.calls().iter().any(|c| c.starts_with("UpdateNotifySettings chats ")),
        "expected UpdateNotifySettings chats on wire, got: {:#?}",
        world.calls()
    );
    // Unset group row g8001 should now inherit muted = true.
    let crew = world.until("g8001 muted after group default changed", || {
        match world.ask(Request::Conversation { conversation: key("g8001") }) {
            Response::Conversation { conversation } if conversation.muted => Some(conversation),
            _ => None,
        }
    });
    assert!(crew.muted);

    // Reset all defaults and exceptions.
    let after_reset = match world.ask(Request::ResetNotifyDefaults { account: account() }) {
        Response::NotifyDefaults { defaults } => defaults,
        other => panic!("expected NotifyDefaults, got {other:?}"),
    };
    assert!(!after_reset.users_muted && !after_reset.groups_muted && !after_reset.channels_muted);
    assert!(after_reset.exceptions.is_empty());
    assert!(
        world.calls().iter().any(|c| c == "ResetNotifySettings"),
        "expected ResetNotifySettings on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn saved_messages_dialogs_history_and_reaction_tags() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.saved_subdialogs, "Telegram must advertise saved_subdialogs capability");

    // Fetch Saved Messages sub-dialogs (by source peer) and reaction tags.
    let (dialogs, tags) = match world.ask(Request::SavedDialogs { account: account() }) {
        Response::SavedDialogs { dialogs, tags } => (dialogs, tags),
        other => panic!("expected SavedDialogs, got {other:?}"),
    };
    assert!(
        dialogs.iter().any(|d| d.peer_id == "u7002" && d.title.contains("Odalys")),
        "expected Odalys saved dialog, got {dialogs:?}"
    );
    assert!(
        dialogs.iter().any(|d| d.peer_id == "c9001" && d.title.contains("Field Notes")),
        "expected Field Notes saved dialog, got {dialogs:?}"
    );
    assert!(
        tags.iter().any(|t| t.emoji == "⭐" && t.title == "Gear" && t.count == 2),
        "expected ⭐ Gear tag, got {tags:?}"
    );

    // Fetch Saved Messages history for a specific source peer (`messages.getSavedHistory`).
    let peer_msgs = match world.ask(Request::SavedHistory {
        account: account(),
        peer_id: Some("u7002".into()),
        tag: None,
        limit: 20,
    }) {
        Response::SavedHistory { messages } => messages,
        other => panic!("expected SavedHistory, got {other:?}"),
    };
    assert_eq!(peer_msgs.len(), 1);
    assert!(peer_msgs[0].text.contains("Trailhead"), "got: {:?}", peer_msgs[0].text);
    assert!(
        world.calls().iter().any(|c| c == "GetSavedHistory u7002"),
        "expected GetSavedHistory u7002 on wire, got: {:#?}",
        world.calls()
    );

    // Fetch Saved Messages filtered by reaction tag (`messages.search` with `saved_reaction`).
    let tag_msgs = match world.ask(Request::SavedHistory {
        account: account(),
        peer_id: None,
        tag: Some("⭐".into()),
        limit: 20,
    }) {
        Response::SavedHistory { messages } => messages,
        other => panic!("expected SavedHistory, got {other:?}"),
    };
    assert_eq!(tag_msgs.len(), 1);
    assert!(tag_msgs[0].text.contains("Packing list"), "got: {:?}", tag_msgs[0].text);
    assert!(
        world.calls().iter().any(|c| c == "SearchSavedTag ⭐"),
        "expected SearchSavedTag ⭐ on wire, got: {:#?}",
        world.calls()
    );
}

#[test]
fn active_sessions_list_and_end_non_current() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.active_sessions, "Telegram must advertise active_sessions capability");

    // List all active sessions (3 seeded: 1 current + 2 other devices).
    let sessions = match world.ask(Request::ActiveSessions { account: account() }) {
        Response::ActiveSessions { sessions } => sessions,
        other => panic!("expected ActiveSessions, got {other:?}"),
    };
    assert_eq!(sessions.len(), 3, "expected 3 active sessions, got {sessions:?}");
    assert!(sessions.iter().any(|s| s.current && s.id == "0"));
    assert!(sessions.iter().any(|s| !s.current && s.id == "91001" && s.device.contains("iPhone")));

    // Refuses ending the current session (hash 0).
    let refuse = world.ask(Request::EndSession {
        account: account(),
        session_id: "0".into(),
    });
    assert!(matches!(refuse, Response::Error { .. }), "ending current session must be refused, got {refuse:?}");

    // End a non-current session (91001).
    let ok = world.ask(Request::EndSession {
        account: account(),
        session_id: "91001".into(),
    });
    assert!(matches!(ok, Response::Ok), "expected Ok ending session 91001, got {ok:?}");
    assert!(
        world.calls().iter().any(|c| c == "ResetAuthorization 91001"),
        "expected ResetAuthorization 91001 on wire, got: {:#?}",
        world.calls()
    );

    // Re-listing sessions should no longer contain 91001.
    let after = match world.ask(Request::ActiveSessions { account: account() }) {
        Response::ActiveSessions { sessions } => sessions,
        other => panic!("expected ActiveSessions, got {other:?}"),
    };
    assert_eq!(after.len(), 2);
    assert!(!after.iter().any(|s| s.id == "91001"));
}

#[test]
fn unread_mentions_jump_three_times_and_the_count_reaches_zero() {
    let world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.unread_jumps, "Telegram must advertise unread_jumps capability");

    let crew = key("g8001");
    // Three unread mentions (21, 22, 23) and two unread reactions (24, 25) on Trail crew.
    // A topic mention and a channel mention ride the same tell, for top_msg_id and channels.readMessageContents.
    world.steer("unread-jumps");
    let row = world.until("the mention and reaction counts on the row", || {
        let conversation = world.conversation("g8001")?;
        (conversation.mentions == 3 && conversation.unread_reactions == 2).then_some(conversation)
    });
    assert_eq!(row.mentions, 3);
    assert_eq!(row.unread_reactions, 2);

    // Each press: the next unread mention, a HistoryAround landing, then that mention's contents read.
    for (step, id) in ["21", "22", "23"].into_iter().enumerate() {
        let left = 3 - step as u32;
        let mention_ids = match world.ask(Request::UnreadItems {
            conversation: crew.clone(),
            kind: protocol::UnreadItemKind::Mentions,
            limit: 1,
        }) {
            Response::UnreadItems { message_ids } => message_ids,
            other => panic!("expected UnreadItems, got {other:?}"),
        };
        assert_eq!(mention_ids, vec![id.to_owned()], "press {} should land on the next mention", step + 1);
        assert!(
            world.calls().iter().any(|call| call == "GetUnreadMentions g8001"),
            "expected GetUnreadMentions g8001 on wire, got: {:#?}",
            world.calls()
        );

        let _ = world.ask(Request::Messages {
            conversation: crew.clone(),
            page: Page::Around { id: id.to_owned() },
            limit: 50,
        });
        let landing = format!("GetHistory g8001 {id} -25");
        world.until(&format!("the landing on {id}"), || {
            world.calls().iter().any(|call| call == &landing).then_some(())
        });
        world.until(&format!("message {id} in the landing"), || world.message("g8001", id));

        let ok = world.ask(Request::MarkUnreadItemsRead {
            conversation: crew.clone(),
            kind: protocol::UnreadItemKind::Mentions,
            message_ids: vec![id.into()],
        });
        assert!(matches!(ok, Response::Ok), "expected Ok marking mention {id} read, got {ok:?}");
        let contents = format!("ReadMessageContents {id}");
        assert!(
            world.calls().iter().any(|call| call == &contents),
            "expected {contents} on wire, got: {:#?}",
            world.calls()
        );
        let now = world.until(&format!("the mention count to fall to {}", left - 1), || {
            world.conversation("g8001").filter(|conversation| conversation.mentions == left - 1)
        });
        assert_eq!(now.mentions, left - 1);
    }
    assert_eq!(world.conversation("g8001").unwrap().mentions, 0);

    // Mark all is a separate call from seeing one mention.
    let ok = world.ask(Request::MarkUnreadItemsRead {
        conversation: crew.clone(),
        kind: protocol::UnreadItemKind::Mentions,
        message_ids: Vec::new(),
    });
    assert!(matches!(ok, Response::Ok), "expected Ok marking all mentions read, got {ok:?}");
    assert!(
        world.calls().iter().any(|call| call == "ReadMentions g8001"),
        "expected ReadMentions g8001 on wire, got: {:#?}",
        world.calls()
    );

    // A forum topic sends top_msg_id (the conversation key carries it on the wire log).
    let topic_ids = match world.ask(Request::UnreadItems {
        conversation: key("c9002_31"),
        kind: protocol::UnreadItemKind::Mentions,
        limit: 1,
    }) {
        Response::UnreadItems { message_ids } => message_ids,
        other => panic!("expected UnreadItems, got {other:?}"),
    };
    assert_eq!(topic_ids, vec!["42".to_owned()]);
    assert!(
        world.calls().iter().any(|call| call == "GetUnreadMentions c9002_31"),
        "expected GetUnreadMentions c9002_31 (top_msg_id) on wire, got: {:#?}",
        world.calls()
    );

    // A channel mention on screen uses channels.readMessageContents.
    let ok = world.ask(Request::MarkUnreadItemsRead {
        conversation: key("c9001"),
        kind: protocol::UnreadItemKind::Mentions,
        message_ids: vec!["5".into()],
    });
    assert!(matches!(ok, Response::Ok), "expected Ok marking the channel mention read, got {ok:?}");
    assert!(
        world.calls().iter().any(|call| call == "channels.ReadMessageContents c9001 5"),
        "expected channels.ReadMessageContents c9001 5 on wire, got: {:#?}",
        world.calls()
    );

    // Reactions: the row's count, then mark-all (`messages.readReactions`) clears it.
    let reaction_ids = match world.ask(Request::UnreadItems {
        conversation: crew.clone(),
        kind: protocol::UnreadItemKind::Reactions,
        limit: 20,
    }) {
        Response::UnreadItems { message_ids } => message_ids,
        other => panic!("expected UnreadItems, got {other:?}"),
    };
    assert_eq!(reaction_ids, vec!["24".to_owned(), "25".to_owned()]);
    assert!(
        world.calls().iter().any(|call| call == "GetUnreadReactions g8001"),
        "expected GetUnreadReactions g8001 on wire, got: {:#?}",
        world.calls()
    );
    let ok = world.ask(Request::MarkUnreadItemsRead {
        conversation: crew.clone(),
        kind: protocol::UnreadItemKind::Reactions,
        message_ids: Vec::new(),
    });
    assert!(matches!(ok, Response::Ok), "expected Ok marking reactions read, got {ok:?}");
    assert!(
        world.calls().iter().any(|call| call == "ReadReactions g8001"),
        "expected ReadReactions g8001 on wire, got: {:#?}",
        world.calls()
    );
    let cleared = world.until("the reaction count to reach zero", || {
        world.conversation("g8001").filter(|conversation| conversation.unread_reactions == 0 && conversation.mentions == 0)
    });
    assert_eq!(cleared.unread_reactions, 0);
    assert_eq!(cleared.mentions, 0);
}

#[test]
fn clicking_a_telegram_username_resolves_once_and_caches_result() {
    let world = World::signed_in();
    world.connected();

    // Resolving an uncached @username calls contacts.resolveUsername once and creates the chat row.
    let Response::Resolved { conversation, title, .. } = world.ask(Request::Resolve {
        url: "https://t.me/forest_wren".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(conversation, Some(key("u7011")));
    assert_eq!(title, "Robin Vale");
    assert_eq!(world.called("ResolveUsername forest_wren"), 1);

    // Clicking the same username again hits cache without repeating the flood-limited RPC.
    let Response::Resolved { conversation: again, .. } = world.ask(Request::Resolve {
        url: "https://t.me/forest_wren".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(again, Some(key("u7011")));
    assert_eq!(world.called("ResolveUsername forest_wren"), 1);

    // Unknown usernames cache the miss so repeated clicks do not hammer ResolveUsername.
    let Response::Resolved { conversation: missing, .. } = world.ask(Request::Resolve {
        url: "https://t.me/nobody_here_495".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(missing, None);
    let Response::Resolved { conversation: missing_again, .. } = world.ask(Request::Resolve {
        url: "https://t.me/nobody_here_495".into(),
    }) else {
        panic!("expected Resolved");
    };
    assert_eq!(missing_again, None);
    assert_eq!(world.called("ResolveUsername nobody_here_495"), 1);
}

#[test]
fn replace_media_and_edit_caption_formatting() {
    let world = World::signed_in();
    world.connected();

    let dir = std::env::temp_dir().join(format!("tinyclient-tg-edit-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let photo_a = dir.join("first.jpg");
    let photo_b = dir.join("second.jpg");
    std::fs::write(&photo_a, vec![3u8; 4_096]).unwrap();
    std::fs::write(&photo_b, vec![9u8; 8_192]).unwrap();

    // Send a photo with a plain caption first.
    let file_a = protocol::OutgoingFile {
        path: photo_a.to_string_lossy().into_owned(),
        ..Default::default()
    };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: key("u7002"),
            client_id: "edit-media-1".into(),
            text: "initial caption".into(),
            reply_to: None,
            with: protocol::Outgoing { files: vec![file_a], ..Default::default() },
        }),
        Response::Sent { .. }
    ));
    let sent = world.until("photo sent", || {
        world
            .messages("u7002")
            .into_iter()
            .find(|m| m.client_id.as_deref() == Some("edit-media-1") && m.state == SendState::Sent)
    });
    assert_eq!(sent.attachments.len(), 1);
    let old_file_id = sent.attachments[0].file.id.clone();

    // Edit only the caption's formatting (bold, italic, spoiler) via Request::Edit.
    assert_eq!(
        world.ask(Request::Edit {
            conversation: key("u7002"),
            message_id: sent.id.clone(),
            text: "**bold** _italic_ ||secret||".into(),
            spans: Vec::new(),
            suppress_embeds: false, keep_attachments: None,
        }),
        Response::Ok
    );
    let formatted = world.until("caption formatted", || {
        world
            .messages("u7002")
            .into_iter()
            .find(|m| m.id == sent.id && m.text == "bold italic secret")
    });
    assert!(formatted.spans.iter().any(|s| s.style == protocol::Style::Bold), "{:?}", formatted.spans);
    assert!(formatted.spans.iter().any(|s| s.style == protocol::Style::Italic), "{:?}", formatted.spans);
    assert!(formatted.spans.iter().any(|s| s.style == protocol::Style::Spoiler), "{:?}", formatted.spans);

    // Clearing the caption on a media message is allowed.
    assert_eq!(
        world.ask(Request::Edit {
            conversation: key("u7002"),
            message_id: sent.id.clone(),
            text: String::new(),
            spans: Vec::new(),
            suppress_embeds: false, keep_attachments: None,
        }),
        Response::Ok
    );
    let cleared = world.until("caption cleared", || {
        world.messages("u7002").into_iter().find(|m| m.id == sent.id && m.text.is_empty())
    });
    assert_eq!(cleared.attachments.len(), 1);

    // Replace the photo with a new file + spoiler and a formatted caption via Request::EditMedia.
    let file_b = protocol::OutgoingFile {
        path: photo_b.to_string_lossy().into_owned(),
        spoiler: true,
        ..Default::default()
    };
    assert_eq!(
        world.ask(Request::EditMedia {
            conversation: key("u7002"),
            message_id: sent.id.clone(),
            file: file_b,
            text: "**new** photo".into(),
            spans: Vec::new(),
            invert_media: true,
        }),
        Response::Ok
    );
    let replaced = world.until("media replaced", || {
        world
            .messages("u7002")
            .into_iter()
            .find(|m| m.id == sent.id && m.text == "new photo")
    });
    assert_eq!(replaced.attachments.len(), 1);
    assert_ne!(replaced.attachments[0].file.id, old_file_id);
    assert!(replaced.attachments[0].spoiler);
    assert!(replaced.spans.iter().any(|s| s.style == protocol::Style::Bold));
    assert!(
        world.calls().iter().any(|c| c.contains("EditMessage u7002") && c.contains("media=photo+spoiler") && c.contains("[invert]")),
        "expected EditMessage with replacement media and invert_media on wire: {:#?}",
        world.calls()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edit_own_profile_name_bio_username_avatar_birthday_and_emoji_status() {
    let world = World::signed_in();
    world.connected();

    let me = key("u7001");
    let account = me.account();

    // Update name, bio, username, and birthday in one SetProfile call.
    assert_eq!(
        world.ask(Request::SetProfile {
            account: account.clone(),
            name: Some("Mira Vance".into()),
            about: Some("Alpine maps and trails".into()),
            pronouns: None,
            emoji: None,
            username: Some("@mira_vance".into()),
            birthday: Some("1994-03-14".into()),
            photo: false,
            accent: None, banner: None, space: None,
        }),
        Response::Ok
    );
    assert_eq!(world.called("UpdateProfile Mira Vance | Alpine maps and trails"), 1);
    assert_eq!(world.called("CheckUsername mira_vance"), 1);
    assert_eq!(world.called("UpdateUsername mira_vance"), 1);
    assert_eq!(world.called("UpdateBirthday 1994-3-14"), 1);

    // Own profile card reflects the updated name, bio, username, and birthday.
    let Response::Profile { profile } = world.ask(Request::Profile { person: me.clone(), space: None }) else {
        panic!("expected Profile");
    };
    assert_eq!(profile.person.name, "Mira Vance");
    assert_eq!(profile.bio, "Alpine maps and trails");
    // The typed profile carries the handle on the person, not as a field.
    assert_eq!(profile.person.handle, "@mira_vance");
    assert!(profile.fields.iter().any(|(k, v)| k == "Birthday" && v == "14 March 1994"), "{:?}", profile.fields);

    // Taken username is checked and refused cleanly without calling UpdateUsername.
    let taken = world.ask(Request::SetProfile {
        account: account.clone(),
        name: None,
        about: None,
        pronouns: None,
        emoji: None,
        username: Some("taken_name".into()),
        birthday: None,
        photo: false,
        accent: None, banner: None, space: None,
    });
    assert!(matches!(taken, Response::Error { code: ErrorCode::Refused, .. }), "{taken:?}");
    assert_eq!(world.called("UpdateUsername taken_name"), 0);

    // Upload a new profile photo and then remove it via SetAvatar.
    let dir = std::env::temp_dir().join(format!("tinyclient-tg-avatar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let avatar = dir.join("me.jpg");
    std::fs::write(&avatar, vec![5u8; 2_048]).unwrap();
    assert_eq!(
        world.ask(Request::SetAvatar {
            account: account.clone(),
            path: avatar.to_string_lossy().into_owned(),
        }),
        Response::Ok
    );
    assert_eq!(world.called("UploadProfilePhoto"), 1);
    assert_eq!(world.ask(Request::SetAvatar { account: account.clone(), path: String::new() }), Response::Ok);
    assert_eq!(world.called("GetUserPhotos"), 1);
    assert_eq!(world.called("DeletePhotos 1"), 1);
    let _ = std::fs::remove_dir_all(&dir);

    // Non-Premium account setting emoji status gets a clear Premium refusal and never sends UpdateStatus offline.
    let emoji_err = world.ask(Request::SetPresence {
        account: account.clone(),
        presence: Presence::Online,
        status: None,
        status_emoji: Some("5002".into()),
        clear_at: Some(1_800_000_000),
    });
    assert!(
        matches!(emoji_err, Response::Error { code: ErrorCode::Refused, ref detail } if detail.contains("Premium")),
        "{emoji_err:?}"
    );
    // Clearing emoji status succeeds cleanly.
    assert_eq!(
        world.ask(Request::SetPresence {
            account: account.clone(),
            presence: Presence::Online,
            status: None,
            status_emoji: Some(String::new()),
            clear_at: None,
        }),
        Response::Ok
    );
    assert_eq!(world.called("UpdateEmojiStatus clear"), 1);
    assert_eq!(world.called("UpdateStatus offline"), 0, "never send account.updateStatus offline from status edits");
}

#[test]
fn create_group_channel_and_add_members_with_missing_invitees() {
    let world = World::signed_in();
    world.connected();

    let account = key("u7001").account();

    // 1. Create a basic group with two people via StartGroup.
    let Response::Conversation { conversation: group } = world.ask(Request::StartGroup {
        account: account.clone(),
        people: vec![key("u7002"), key("u7003")],
        title: Some("Summit Team".into()),
    }) else {
        panic!("expected Conversation from StartGroup");
    };
    assert_eq!(group.title, "Summit Team");
    assert_eq!(group.kind, ConversationKind::Group);
    assert!(group.rights.add_members);
    assert_eq!(world.called("CreateChat Summit Team users=[7002, 7003]"), 1);
    let group_msgs = world.messages(&group.key.id);
    assert!(
        group_msgs.iter().any(|m| m.kind == MessageKind::System),
        "expected creation system message in new group: {group_msgs:?}"
    );

    // 2. Create a public broadcast channel with description and public @username.
    let Response::Conversation { conversation: pub_chan } = world.ask(Request::CreateChannel {
        space: Key::new(&account, ""),
        name: "Trail Bulletins".into(),
        kind: ConversationKind::Channel,
        private: false,
        group: None,
        description: Some("Weekly snowpack updates".into()),
        username: Some("@trail_bulletins".into()),
    }) else {
        panic!("expected Conversation from public CreateChannel");
    };
    assert_eq!(pub_chan.title, "Trail Bulletins");
    assert_eq!(pub_chan.kind, ConversationKind::Channel);
    assert!(pub_chan.rights.add_members);
    assert_eq!(world.called("CreateChannel Trail Bulletins broadcast=true megagroup=false about=Weekly snowpack updates"), 1);
    assert_eq!(world.called("ChannelCheckUsername trail_bulletins"), 1);
    assert!(
        world.calls().iter().any(|c| c.starts_with("ChannelUpdateUsername") && c.ends_with(" trail_bulletins")),
        "{:#?}",
        world.calls()
    );

    // 3. Create a private channel (must not claim a public username).
    let Response::Conversation { conversation: priv_chan } = world.ask(Request::CreateChannel {
        space: Key::new(&account, ""),
        name: "Private Log".into(),
        kind: ConversationKind::Channel,
        private: true,
        group: None,
        description: None,
        username: Some("should_be_ignored".into()),
    }) else {
        panic!("expected Conversation from private CreateChannel");
    };
    assert_eq!(priv_chan.title, "Private Log");
    assert_eq!(world.called("ChannelCheckUsername should_be_ignored"), 0);

    // 4. Add a member to a basic group and to the new channel.
    assert_eq!(
        world.ask(Request::MembersAdd {
            conversation: key("g8001"),
            people: vec!["u7002".into()],
            share_history: None,
        }),
        Response::Ok
    );
    assert_eq!(world.called("AddChatUser g8001 u7002"), 1);

    assert_eq!(
        world.ask(Request::MembersAdd {
            conversation: pub_chan.key.clone(),
            people: vec!["u7002".into()],
            share_history: None,
        }),
        Response::Ok
    );
    assert!(
        world.calls().iter().any(|c| c.starts_with("InviteToChannel ") && c.contains("[7002]")),
        "{:#?}",
        world.calls()
    );

    // 5. Missing invitees (privacy-restricted person u7005) produce a clear refusal suggesting an invite link.
    let privacy = world.ask(Request::MembersAdd {
        conversation: pub_chan.key.clone(),
        people: vec!["u7005".into()],
        share_history: None,
    });
    assert!(
        matches!(privacy, Response::Error { code: ErrorCode::Refused, ref detail } if detail.contains("invite link")),
        "expected privacy refusal naming invite link, got {privacy:?}"
    );
}

#[test]
fn invite_links_join_preview_revoke_and_join_requests() {
    let world = World::signed_in();
    world.connected();

    let account = key("u7001").account();

    // 1. Preview a normal Telegram invite link without joining.
    let Response::InvitePreview { preview } = world.ask(Request::InvitePreview {
        account: account.clone(),
        code: "https://t.me/+alpine_invite".into(),
        event_id: None,
    }) else {
        panic!("expected InvitePreview");
    };
    assert_eq!(preview.name, "Alpine Club");
    assert_eq!(preview.members, Some(128));
    assert!(!preview.request_needed);
    assert_eq!(world.called("CheckChatInvite alpine_invite"), 1);

    // 2. Join via the invite link: calls ImportChatInvite and records the new channel row.
    let Response::InviteJoined { space, conversation, pending } = world.ask(Request::InviteJoin {
        account: account.clone(),
        code: "https://t.me/+alpine_invite".into(),
    }) else {
        panic!("expected InviteJoined");
    };
    assert_eq!(space, None);
    assert_eq!(conversation, Some(key("c9201")));
    assert!(!pending);
    assert_eq!(world.called("ImportChatInvite alpine_invite"), 1);
    let joined_row = world.conversation("c9201").expect("joined channel row recorded");
    assert_eq!(joined_row.title, "Alpine Club");

    // 3. Approval-gated invite link: preview sets request_needed, join reports pending=true on INVITE_REQUEST_SENT.
    let Response::InvitePreview { preview: gated } = world.ask(Request::InvitePreview {
        account: account.clone(),
        code: "https://t.me/joinchat/approval_invite".into(),
        event_id: None,
    }) else {
        panic!("expected gated InvitePreview");
    };
    assert!(gated.request_needed);
    let Response::InviteJoined { pending: req_sent, .. } = world.ask(Request::InviteJoin {
        account: account.clone(),
        code: "https://t.me/+approval_invite".into(),
    }) else {
        panic!("expected pending InviteJoined");
    };
    assert!(req_sent);

    // 4. Expired invite returns a clear Refused error.
    let expired = world.ask(Request::InvitePreview {
        account: account.clone(),
        code: "https://t.me/+expired_invite".into(),
        event_id: None,
    });
    assert!(
        matches!(expired, Response::Error { code: ErrorCode::Refused, ref detail } if detail.contains("expired")),
        "{expired:?}"
    );

    // 5. Admin invite management: create, list active, revoke, list revoked.
    let Response::ChatInvite { invite: created } = world.ask(Request::CreateInvite {
        conversation: key("g8001"),
        title: Some("Trail guests".into()),
        expire_date: Some(1_900_000_000),
        usage_limit: Some(10),
        request_needed: false,
    }) else {
        panic!("expected ChatInvite from CreateInvite");
    };
    assert!(created.link.starts_with("https://t.me/+"));
    assert!(!created.revoked);
    assert_eq!(created.usage_limit, Some(10));

    let Response::ChatInvites { invites: active, .. } = world.ask(Request::ChatInvites {
        conversation: key("g8001"),
        revoked: false,
    }) else {
        panic!("expected ChatInvites");
    };
    assert!(active.iter().any(|i| i.link == created.link), "{active:?}");

    let Response::ChatInvite { invite: revoked } = world.ask(Request::RevokeInvite {
        conversation: key("g8001"),
        link: created.link.clone(),
    }) else {
        panic!("expected ChatInvite from RevokeInvite");
    };
    assert!(revoked.revoked);

    // 6. Admin join request management: list requests, approve one, decline remaining.
    let Response::JoinRequests { requests, .. } = world.ask(Request::JoinRequests {
        conversation: key("g8001"),
        link: None,
        limit: 50,
    }) else {
        panic!("expected JoinRequests");
    };
    assert_eq!(requests.len(), 2, "{requests:?}");

    assert_eq!(
        world.ask(Request::HandleJoinRequest {
            conversation: key("g8001"),
            user_id: Some("u7002".into()),
            approved: true,
            link: None,
        }),
        Response::Ok
    );
    assert_eq!(world.called("HideChatJoinRequest g8001 u7002 approved=true"), 1);

    assert_eq!(
        world.ask(Request::HandleJoinRequest {
            conversation: key("g8001"),
            user_id: None,
            approved: false,
            link: None,
        }),
        Response::Ok
    );
    assert_eq!(world.called("HideAllChatJoinRequests g8001 approved=false"), 1);

    let Response::JoinRequests { requests: after, .. } = world.ask(Request::JoinRequests {
        conversation: key("g8001"),
        link: None,
        limit: 50,
    }) else {
        panic!("expected empty JoinRequests");
    };
    assert!(after.is_empty());
}

#[test]
fn a_search_previews_a_public_channel_until_it_is_joined() {
    let world = World::signed_in();
    world.connected();

    // A link is not a join target. The private-invite path is separate, and this must not subscribe.
    let joins = world.called("JoinChannel");
    let link = world.ask(Request::Join { conversation: key("https://t.me/larchpress") });
    assert!(matches!(link, Response::Error { code: ErrorCode::Refused, .. }), "{link:?}");
    assert_eq!(world.called("JoinChannel"), joins);

    let search = |query: &str| Request::People {
        network: Some(Network::Telegram), relation: None, query: Some(query.into()), offset: 0, limit: 20,
    };
    world.ask(search("larch"));
    world.until("the channel search", || world.calls().contains(&"contacts.Search larch".to_owned()).then_some(()));
    let larch = world.until("the channel preview", || {
        world.conversation("c9010").filter(|row| row.title == "Larch Press" && !row.joined)
    });
    assert_eq!(larch.kind, ConversationKind::Channel);
    let trail = world.until("the group preview", || {
        world.conversation("c9014").filter(|row| row.title == "Larch Trail" && row.kind == ConversationKind::Group && !row.joined)
    });
    assert_eq!(trail.title, "Larch Trail");

    let Response::Conversation { conversation: preview } = world.ask(Request::PreviewChannel { conversation: key("c9010") }) else {
        panic!("expected a preview");
    };
    assert!(!preview.joined);
    assert!(world.messages("c9010").iter().any(|message| message.text == "The ridge is clear."));
    assert!(world.calls().iter().any(|call| call.starts_with("GetHistory c9010")));

    let Response::Conversation { conversation: joined } = world.ask(Request::Join { conversation: key("c9010") }) else {
        panic!("expected the joined channel");
    };
    assert!(joined.joined);
    assert_eq!(joined.title, "Larch Press");
    assert_eq!(world.called("JoinChannel c9010"), 1);
    assert!(world.calls().iter().any(|call| call == "GetChannelRecommendations c9010"));
    let similar = world.conversation("c9013").expect("a similar channel stays a preview");
    assert_eq!((similar.title.as_str(), similar.joined), ("Similar Peaks", false));
    assert!(!world.conversation("c9014").expect("the group was not joined").joined);

    world.ask(search("shelf"));
    world.until("the full channel search", || world.calls().contains(&"contacts.Search shelf".to_owned()).then_some(()));
    world.until("the full channel preview", || world.conversation("c9012").filter(|row| row.title == "Full Shelf" && !row.joined).map(drop));
    let full = world.ask(Request::Join { conversation: key("c9012") });
    assert!(
        matches!(full, Response::Error { code: ErrorCode::Refused, ref detail } if detail == "This account can't join any more channels."),
        "{full:?}"
    );
    assert!(!world.conversation("c9012").expect("still a preview").joined);

    world.ask(search("closed"));
    world.until("the closed group search", || world.calls().contains(&"contacts.Search closed".to_owned()).then_some(()));
    world.until("the closed group preview", || world.conversation("c9011").filter(|row| row.title == "Closed Ridge" && !row.joined).map(drop));
    let closed = world.ask(Request::Join { conversation: key("c9011") });
    assert!(
        matches!(closed, Response::Error { code: ErrorCode::Refused, ref detail } if detail == "Request sent."),
        "{closed:?}"
    );
    assert!(!world.conversation("c9011").expect("a request does not join").joined);
}

#[test]
fn voice_and_round_video_messages_track_listened_state_and_send_read_contents() {
    let world = World::signed_in();
    world.connected();

    // 1. Incoming voice message with media_unread arrives flagged unplayed.
    world.steer("voice-unread");
    let voice = world.arrived("u7002", "61");
    assert!(voice.media_unread, "unplayed incoming voice message must carry media_unread");
    assert!(!voice.outgoing);

    // Playing it sends messages.readMessageContents once and clears media_unread.
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("u7002"), message_id: "61".into() }),
        Response::Ok
    ));
    world.until("media_unread cleared on voice 61", || {
        world.message("u7002", "61").filter(|m| !m.media_unread)
    });
    assert_eq!(world.called("ReadMessageContents 61"), 1, "expected exactly one ReadMessageContents call");

    // Asking MarkPlayed a second time sends nothing further.
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("u7002"), message_id: "61".into() }),
        Response::Ok
    ));
    assert_eq!(world.called("ReadMessageContents 61"), 1, "must not send ReadMessageContents twice");

    // 2. Incoming round video message with media_unread arrives flagged and clears on MarkPlayed.
    world.steer("round-unread");
    let round = world.arrived("u7002", "65");
    assert!(round.media_unread, "unplayed round video must carry media_unread");
    assert!(round.attachments.first().is_some_and(|a| a.round));
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("u7002"), message_id: "65".into() }),
        Response::Ok
    ));
    world.until("media_unread cleared on round 65", || {
        world.message("u7002", "65").filter(|m| !m.media_unread)
    });
    assert_eq!(world.called("ReadMessageContents 65"), 1);

    // 3. Channel voice message sends channels.readMessageContents and clears on update too.
    world.steer("channel-voice-unread");
    let ch_voice = world.arrived("c9001", "64");
    assert!(ch_voice.media_unread);
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("c9001"), message_id: "64".into() }),
        Response::Ok
    ));
    world.until("media_unread cleared on channel voice 64", || {
        world.message("c9001", "64").filter(|m| !m.media_unread)
    });
    assert_eq!(world.called("channels.ReadMessageContents c9001 64"), 1);

    // 4. Outgoing unplayed voice message: playing it locally does NOT send readMessageContents
    //    or clear the dot; UpdateReadMessagesContents from the recipient clears it.
    world.steer("voice-out-unread");
    let mine = world.arrived("u7002", "63");
    assert!(mine.outgoing && mine.media_unread);
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("u7002"), message_id: "63".into() }),
        Response::Ok
    ));
    assert_eq!(world.called("ReadMessageContents 63"), 0, "must not mark own outgoing message played");
    assert!(world.message("u7002", "63").unwrap().media_unread);
    world.steer("read-contents 63");
    world.until("UpdateReadMessagesContents clears outgoing voice 63", || {
        world.message("u7002", "63").filter(|m| !m.media_unread)
    });

    // 5. Unplayed voice message with a mention decrements the conversation's mention count once.
    world.steer("voice-unread-mention");
    let mentioned = world.arrived("g8001", "62");
    assert!(mentioned.media_unread && mentioned.mentions_me);
    let before_mentions = world.conversation("g8001").unwrap().mentions;
    assert!(before_mentions >= 1, "expected mention count >= 1, got {before_mentions}");
    assert!(matches!(
        world.ask(Request::MarkPlayed { conversation: key("g8001"), message_id: "62".into() }),
        Response::Ok
    ));
    world.until("mention count decremented after readMessageContents", || {
        let now_mentions = world.conversation("g8001")?.mentions;
        (now_mentions == before_mentions - 1).then_some(now_mentions)
    });
    // Echoed UpdateReadMessagesContents must not double-decrement mentions.
    world.steer("read-contents 62");
    assert_eq!(world.conversation("g8001").unwrap().mentions, before_mentions - 1);
}

#[test]
fn send_rules_reflect_media_bans_slow_mode_and_paid_stars() {
    let world = World::signed_in();
    world.connected();
    // The dialog list only previews the forum. Opening it lists the topic.
    let _ = world.messages("c9002");
    world.until("the tools topic", || world.conversation("c9002_31"));

    // 1. Group with media banned + 30s slow mode reports those rules and starts countdown on send.
    world.steer("ban-media");
    world.steer("slowmode 30");
    let Response::SendRules { rules, .. } = world.ask(Request::SendRules { conversation: key("c9002_31") }) else {
        panic!("expected SendRules");
    };
    assert!(!rules.send_media && !rules.allows_any_attachment());
    assert!(!rules.send_stickers && !rules.send_gifs && !rules.send_voices);
    assert!(rules.send_messages && rules.send_plain);
    assert_eq!(rules.slowmode_seconds, 30);
    assert_eq!(rules.slowmode_next_send, None);

    // Sending a message in the slow-mode chat sets the cached next-send timestamp.
    world.send("c9002_31", "5351", "Checked the bearings", None);
    let _ = world.until("slowmode message sent", || {
        world.messages("c9002_31").into_iter().find(|m| m.text == "Checked the bearings" && m.state == SendState::Sent)
    });
    let Response::SendRules { rules: after_send, .. } = world.ask(Request::SendRules { conversation: key("c9002_31") }) else {
        panic!("expected SendRules after send");
    };
    assert!(after_send.slowmode_next_send.is_some(), "slow mode cooldown must start after send");

    // 2. Paid messages (send_paid_messages_stars): price is reported, silent send fails, explicit
    //    allow_paid_stars succeeds and passes allow_paid_stars on the wire.
    world.steer("paid-stars 25");
    let Response::SendRules { rules: paid_rules, .. } = world.ask(Request::SendRules { conversation: key("u7003") }) else {
        panic!("expected SendRules for paid user");
    };
    assert_eq!(paid_rules.paid_stars, Some(25));

    // Sending without allow_paid_stars fails with a plain-English Stars error.
    world.send("u7003", "5352", "unpaid hello", None);
    let failed = world.until("unpaid send fails", || {
        world.messages("u7003").into_iter().find(|m| {
            m.text == "unpaid hello" && matches!(m.state, SendState::Failed { .. })
        })
    });
    let SendState::Failed { reason } = failed.state else { unreachable!() };
    assert!(reason.contains("25 Telegram Stars"), "{reason}");

    // Sending with explicit allow_paid_stars succeeds and passes allow_paid_stars on the wire.
    let res = world.ask(Request::Send {
        conversation: key("u7003"),
        client_id: "5353".into(),
        text: "paid hello".into(),
        reply_to: None,
        with: protocol::Outgoing { allow_paid_stars: Some(25), ..protocol::Outgoing::default() },
    });
    assert!(matches!(res, Response::Sent { .. }), "{res:?}");
    let _ = world.until("paid send succeeds", || {
        world.messages("u7003").into_iter().find(|m| m.text == "paid hello" && m.state == SendState::Sent)
    });
    assert!(
        world.calls().iter().any(|c| c.contains("SendMessage u7003 paid hello") && c.contains("[stars=25]")),
        "{:#?}",
        world.calls()
    );
}

#[test]
fn voice_messages_forbidden_disables_mic_and_refused_send_returns_plain_words() {
    let world = World::signed_in();
    world.connected();
    world.until("history", || world.message("u7002", "12"));

    // 1. Before full profile is known, Odalys forbids voice messages on the server: sending fails
    //    with plain words and updates the conversation's `voice_notes_forbidden` field.
    world.steer("voice-forbidden 7002 1");
    let dir = std::path::absolute(format!("/tmp/tinyclient-tg-voice-forbid-{}", std::process::id())).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("voice-refused.opus");
    std::fs::write(&path, vec![7u8; 1024]).unwrap();
    let file = || protocol::OutgoingFile {
        path: path.to_string_lossy().into_owned(),
        name: String::new(),
        spoiler: false,
        as_file: false,
        original: false,
        view_once: false,
        media: None,
    };
    let voice = || protocol::VoiceNote { duration: 4_000, waveform: vec![120, 60, 200, 30] };
    assert!(matches!(
        world.ask(Request::Send {
            conversation: key("u7002"),
            client_id: "vf-1".into(),
            text: String::new(),
            reply_to: None,
            with: protocol::Outgoing { files: vec![file()], voice: Some(voice()), ..Default::default() },
        }),
        Response::Sent { .. }
    ));
    let failed = world.until("voice send refused", || {
        world.messages("u7002").into_iter().find(|m| m.client_id.as_deref() == Some("vf-1") && m.state != SendState::Pending)
    });
    match &failed.state {
        SendState::Failed { reason } => {
            assert!(
                reason.contains("doesn't accept voice and video messages"),
                "expected plain-words refusal, got: {reason}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let updated = world.until("conversation marks voice_notes_forbidden", || {
        world.conversation("u7002").filter(|c| !c.allows_voice_notes())
    });
    assert!(updated.voice_notes_forbidden.contains("Odalys Brandt"), "{}", updated.voice_notes_forbidden);

    // 2. Fetching full profile for Tobin (u7003) with voice_messages_forbidden sets the flag proactively,
    //    so chatd refuses a subsequent voice send before it reaches the wire.
    world.steer("voice-forbidden 7003 1");
    assert!(matches!(
        world.ask(Request::ConversationInfo { conversation: key("u7003") }),
        Response::ConversationInfo { .. }
    ));
    let tobin = world.until("tobin voice_notes_forbidden", || {
        world.conversation("u7003").filter(|c| !c.allows_voice_notes())
    });
    assert!(tobin.voice_notes_forbidden.contains("Tobin Reyes"), "{}", tobin.voice_notes_forbidden);
    let calls_before = world.called("SendMedia u7003");
    let refused = world.ask(Request::Send {
        conversation: key("u7003"),
        client_id: "vf-2".into(),
        text: String::new(),
        reply_to: None,
        with: protocol::Outgoing { files: vec![file()], voice: Some(voice()), ..Default::default() },
    });
    match refused {
        Response::Error { code: ErrorCode::Refused, detail } => {
            assert!(detail.contains("doesn't accept voice and video messages"), "{detail}");
        }
        other => panic!("expected immediate Refused, got {other:?}"),
    }
    assert_eq!(world.called("SendMedia u7003"), calls_before, "proactively blocked send must not hit Telegram");

    // 3. Group with send_voices banned fails with plain group wording and records the restriction.
    world.steer("voice-ban g8001 1");
    assert!(matches!(
        world.ask(Request::Send {
            conversation: key("g8001"),
            client_id: "vf-3".into(),
            text: String::new(),
            reply_to: None,
            with: protocol::Outgoing { files: vec![file()], voice: Some(voice()), ..Default::default() },
        }),
        Response::Sent { .. }
    ));
    let group_failed = world.until("group voice send refused", || {
        world.messages("g8001").into_iter().find(|m| m.client_id.as_deref() == Some("vf-3") && m.state != SendState::Pending)
    });
    match &group_failed.state {
        SendState::Failed { reason } => {
            assert!(reason.contains("aren't allowed in this group"), "{reason}");
        }
        other => panic!("expected Failed for group, got {other:?}"),
    }
    let group_row = world.until("group marks voice_notes_forbidden", || {
        world.conversation("g8001").filter(|c| !c.allows_voice_notes())
    });
    assert!(group_row.voice_notes_forbidden.contains("aren't allowed in this group"), "{}", group_row.voice_notes_forbidden);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn voice_transcription_streams_update_persists_across_restart_and_reports_trial_exhausted() {
    let mut world = World::signed_in();
    world.connected();
    let can = world.status().0.into_iter().find(|a| a.key == account()).unwrap().can;
    assert!(can.transcribe, "Telegram must advertise transcribe capability");

    // Incoming voice message (id 15 in u7002 via `steer("voice")`).
    world.steer("voice");
    let voice_msg = world.until("incoming voice message", || {
        world.messages("u7002").into_iter().find(|m| m.attachments.iter().any(|a| !a.waveform.is_empty()))
    });
    assert!(voice_msg.attachments[0].transcription.is_none(), "never transcribe automatically");
    assert_eq!(world.called("TranscribeAudio"), 0, "no automatic transcription call");

    // 1. Pending transcription response followed by `updateTranscribedAudio` completing the text.
    world.steer("transcribe-mode pending 2");
    let resp = world.ask(Request::Transcribe {
        conversation: key("u7002"),
        message_id: voice_msg.id.clone(),
    });
    let pending = match resp {
        Response::Transcription { transcription } => transcription,
        other => panic!("expected Transcription, got {other:?}"),
    };
    assert!(pending.pending);
    assert_eq!(pending.id, 901);
    assert_eq!(pending.text, "Meet at the");
    assert_eq!(pending.trial_remains, Some(1));
    assert!(pending.trial_resets_at.is_some());
    assert_eq!(world.called(&format!("TranscribeAudio u7002 {}", voice_msg.id)), 1);

    // Final part arrives via updateTranscribedAudio.
    world.steer(&format!("transcribe-update u7002 {} 0 Meet at the trailhead at seven.", voice_msg.id));
    let completed = world.until("transcription completed by update", || {
        let msg = world.message("u7002", &voice_msg.id)?;
        let tr = msg.attachments.first()?.transcription.clone()?;
        (!tr.pending && tr.text == "Meet at the trailhead at seven.").then_some(tr)
    });
    assert_eq!(completed.id, 901);
    assert_eq!(completed.trial_remains, Some(1));

    // Rate thumbs up via messages.rateTranscribedAudio.
    assert_eq!(
        world.ask(Request::RateTranscript {
            conversation: key("u7002"),
            message_id: voice_msg.id.clone(),
            transcription_id: 901,
            good: true,
        }),
        Response::Ok
    );
    assert_eq!(world.called(&format!("RateTranscribedAudio u7002 {} 901 true", voice_msg.id)), 1);

    // Persisted in store across chatd restart.
    world.restart();
    world.connected();
    let after_restart = world.until("transcription survives restart", || {
        let msg = world.message("u7002", &voice_msg.id)?;
        let tr = msg.attachments.first()?.transcription.clone()?;
        (tr.text == "Meet at the trailhead at seven." && tr.rated == Some(true)).then_some(tr)
    });
    assert!(!after_restart.pending);

    // 2. Trial exhausted (PREMIUM_ACCOUNT_REQUIRED) records the Premium explanation.
    world.steer("transcribe-mode exhausted 0");
    let resp = world.ask(Request::Transcribe {
        conversation: key("u7002"),
        message_id: voice_msg.id.clone(),
    });
    let exhausted = match resp {
        Response::Transcription { transcription } => transcription,
        other => panic!("expected Transcription on exhausted trial, got {other:?}"),
    };
    assert!(!exhausted.pending);
    assert_eq!(exhausted.trial_remains, Some(0));
    assert!(
        exhausted.failed.as_deref().unwrap_or("").contains("Telegram Premium"),
        "expected Premium explanation, got: {:?}",
        exhausted.failed
    );
}
