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
    MediaKind, MentionKind, Message, MessageKind, Network, Page, Presence, Relation, Request, Response, SendState,
    SignInInput, SignInProblem, SignInStep, Style, Watching, Shelf, Special, Mark, ReactionSet,
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_chatd"));
        command
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_TELEGRAM_FAKE", qr_seconds)
            .env_remove("TINYCLIENT_SESSION_DISCORD")
            .env_remove("TINYCLIENT_SESSION_SLACK")
            .env_remove("TINYCLIENT_SESSION_TELEGRAM")
            .env_remove("TINYCLIENT_TELEGRAM_MANY")
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
    let landed = world.until("the thumbnail on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["p6014m".into()], size: None, retry: false }) {
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
        match world.ask(Request::Fetch { account: account(), ids: vec!["p6014y".into()], size: Some(2_560), retry: false }) {
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
        match world.ask(Request::Fetch { account: account(), ids: vec!["d6100".into()], size: None, retry: false }) {
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
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false }) {
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
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false }) {
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
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5001".into()], size: None, retry: false }) {
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
    assert_eq!(said, [("50", "Tobin Reyes", "On my way up front?"), ("51", "Odalys Brandt", "Yes, at the hut now.")]);
    assert_eq!(world.called("GetReplies c9001_5"), 1, "{:#?}", world.calls());
    assert_eq!(world.called("GetHistory c9001_5"), 0);

    // The newest comment's time rides the post's footer, as "last reply", once the page was
    // read: the section itself names no date. Comment 51 went at 2026-10-13 11:59:20 UTC.
    let fresh = world.until("the footer's last reply", || {
        world.message("c9001", "5").filter(|post| post.thread.as_ref().is_some_and(|section| section.last == 1_791_892_760))
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
    world.until("the discussion to be asked", || (world.called("GetDiscussionMessage c9001_5") == 1).then_some(()));

    // The send is Telegram's own for the discussion group: not the channel, and under the
    // echo of the post itself, the way the phone's apps write a comment.
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
        world.message("c9001_5", "51").filter(|comment| !comment.reactions.is_empty())
    });
    let seen: Vec<(&str, &str)> = reacted.reactions.iter().map(|reaction| (reaction.emoji.as_str(), reaction.file.as_ref().map_or("", |file| file.id.as_str()))).collect();
    assert_eq!(seen, vec![("👍", ""), (":emoji:", "e5002")]);
    assert!(world.called("GetCustomEmojiDocuments 5002") > before, "{:#?}", world.calls());

    // The file lands whole under the name the reaction carries, as any reaction's does.
    let landed = world.until("the comment's reaction picture on disk", || {
        match world.ask(Request::Fetch { account: account(), ids: vec!["e5002".into()], size: None, retry: false }) {
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
        world.message("c9001_5", "51").filter(|comment| !comment.reactions.is_empty())
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
    // g8001 was already read up to 20, so there was nothing to tell for it. Being here is said
    // once at connect, before any acting, and never again: acting's own here() is throttled.
    let calls = world.calls();
    let connect_here = calls.iter().position(|call| call == "UpdateStatus");
    let acting = calls.iter().position(|call| call.starts_with("SetTyping u7003"));
    assert!(connect_here.is_some_and(|at| acting.is_some_and(|last| at < last)), "the connect must say we are here before acting does");
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

    let react = |id: &str, message: &str, emoji: &str| world.ask(Request::React { conversation: key(id), message_id: message.into(), emoji: emoji.into(), on: true });
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
    assert_eq!((profile.nickname.as_str(), profile.joined), ("Quartermaster", Some(1_791_891_800)));
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
    assert_eq!(world.ask(Request::SetMute { conversation: key("g8001"), notify: protocol::Notify::Nothing }), Response::Ok);
    assert_eq!(world.called("UpdateNotifySettings g8001 2147483647"), 1);
    assert!(world.conversation("g8001").unwrap().muted);
    assert_eq!(world.ask(Request::SetMute { conversation: key("g8001"), notify: protocol::Notify::All }), Response::Ok);
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
    assert_eq!(world.ask(Request::Leave { conversation: key("c9001") }), Response::Ok);
    assert_eq!(world.called("LeaveChannel c9001"), 1);
    assert!(world.conversation("c9001").is_none());
    // A direct chat cannot be left.
    assert!(matches!(world.ask(Request::Leave { conversation: key("u7002") }), Response::Error { .. }));
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
    let one = |path: String, spoiler: bool, as_file: bool| protocol::OutgoingFile { path, name: String::new(), spoiler, as_file, original: false };
    let photo = file("ridge.jpg", 700 * 1024);
    let sent = send("8105", "the ridge", protocol::Outgoing { files: vec![one(photo.clone(), true, false)], ..Default::default() });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert_eq!((world.called("SaveFilePart 0 524288"), world.called("SaveFilePart 1 192512")), (1, 1));
    assert_eq!(world.called("SendMedia u7002 photo+spoiler the ridge"), 1);
    let with = vec![one(photo.clone(), false, false), one(file("map.png", 1_000), false, false), one(file("plan.pdf", 2_000), false, true)];
    let album = send("8106", "trip", protocol::Outgoing { files: with, ..Default::default() });
    assert_eq!(album.state, SendState::Sent);
    assert_eq!((world.called("UploadMedia photo"), world.called("SendMultiMedia u7002 kept-photo,kept-photo")), (2, 1));
    // The album is the message, and shows as sent once it lands; the document goes after it.
    world.until("the document after the album", || (world.called("SendMedia u7002 file:application/pdf") > 0).then_some(()));
    assert_eq!(world.called("SendMedia u7002 file:application/pdf"), 1);
    // A big file goes in big parts.
    let big = file("track.gpx", 10 * 1024 * 1024 + 5);
    assert_eq!(send("8107", "", protocol::Outgoing { files: vec![one(big, false, false)], ..Default::default() }).state, SendState::Sent);
    assert_eq!((world.called("SaveBigFilePart 0/21 524288"), world.called("SaveBigFilePart 20/21 5")), (1, 1));
    let _ = std::fs::remove_dir_all(&dir);
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
    let one = protocol::OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: false, as_file: false, original: false };
    // Bars at the widest and shut: both are exact through Telegram's five bits a bar.
    let voice = protocol::VoiceNote { duration: 7_500, waveform: vec![255, 0, 255, 0, 255, 0, 255, 0] };
    let sent = send("8201", "", protocol::Outgoing { files: vec![one], voice: Some(voice), ..Default::default() });
    assert_eq!(sent.state, SendState::Sent, "{:?}", world.calls());
    assert_eq!(world.called("SaveFilePart 0 2048"), 1, "{:?}", world.calls());
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
fn conversation_search_asks_telegram_with_the_query_and_maps_its_hits() {
    let world = World::signed_in();
    world.connected();
    let search = |query: &str| match world.ask(Request::SearchNetwork {
        conversation: Some(key("g8001")), space: None, query: query.into(),
    }) {
        Response::MessageHits { hits } => hits,
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
    let Response::MessageHits { hits } = world.ask(Request::SearchNetwork {
        conversation: Some(key("c9002_31")), space: None, query: "wrench".into(),
    }) else { panic!("no search hits") };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].conversation, key("c9002_31"));
    assert_eq!(hits[0].message_id, "41");
    assert_eq!(world.called("SearchText c9002 wrench topic=Some(31)"), 1);
}
