//! The polish round's shared fixes and contract, through the real socket: chatd answers before
//! the keychain does, survives a panicking request, runs once per directory and answers a frame
//! it cannot read; and the demo answers every list, pane and send the round added.

use chatd::store::{ConversationRow, Store};
use protocol::{
    read_frame, write_frame, AccountKey, ActivityKind, CatalogKind, Change, ChatStream, ContactKind, ConversationKind,
    ErrorCode, InboxFilter, Key, Link, Mark, Message, Network, Notify, Outgoing, OutgoingFile, Request, Response,
    SendState, Shelf, Special, Watching,
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
    fn start(args: &[&str], env: &[(&str, &str)], seed: impl FnOnce(&mut Store)) -> Daemon {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Short: a unix socket address holds about 100 bytes.
        let dir = PathBuf::from(format!("/tmp/tinychat-pl-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        seed(&mut Store::open(&dir.join("chat.sqlite")).unwrap());
        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .args(args)
            .env("TINYCHAT_DIR", &dir)
            .envs(env.iter().copied())
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

    fn demo(env: &[(&str, &str)]) -> Daemon {
        Daemon::start(&["--seed-demo"], env, |_| {})
    }

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    fn until<T>(&self, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = look() {
                return found;
            }
            assert!(Instant::now() < deadline, "{what} did not happen");
            sleep(Duration::from_millis(25));
        }
    }

    fn status(&self) -> (Vec<protocol::AccountStatus>, protocol::Keychain, u64, u64) {
        match self.ask(Request::Status) {
            Response::Status { accounts, keychain, seq, started, .. } => (accounts, keychain, seq, started),
            other => panic!("not a status: {other:?}"),
        }
    }

    /// Every change told from `since` for `time`.
    fn changes(&self, mut since: u64, time: Duration) -> Vec<Change> {
        let deadline = Instant::now() + time;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
            stream.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)))).unwrap();
            write_frame(&mut stream, &Request::Wait { since }).unwrap();
            let Ok(Response::Changes { seq, changes, .. }) = read_frame(&mut stream) else {
                break;
            };
            since = seq;
            seen.extend(changes);
        }
        seen
    }

    fn rows(&self, request: Request) -> (Vec<protocol::Conversation>, protocol::InboxCounts) {
        match self.ask(request) {
            Response::Conversations { conversations, counts, .. } => (conversations, counts),
            other => panic!("not rows: {other:?}"),
        }
    }

    fn row(&self, conversation: &Key) -> protocol::Conversation {
        match self.ask(Request::Conversation { conversation: conversation.clone() }) {
            Response::Conversation { conversation } => conversation,
            other => panic!("not a row: {other:?}"),
        }
    }

    fn message(&self, conversation: &Key, client_id: &str) -> Option<Message> {
        let asked = Request::Messages { conversation: conversation.clone(), page: protocol::Page::Newest, limit: 50 };
        let Response::Messages { messages, .. } = self.ask(asked) else {
            return None;
        };
        messages.into_iter().find(|message| message.client_id.as_deref() == Some(client_id))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn at(network: Network, account: &str, id: &str) -> Key {
    Key { network, account: account.to_owned(), id: id.to_owned() }
}

fn titles(rows: &[protocol::Conversation]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

const ROBIN: &str = "210000000000000000";
const TELEGRAM: &str = "7000000";
const TEAMS: &str = "harbor-tenant";

#[test]
fn the_socket_answers_while_a_keychain_read_waits_and_the_cached_account_stays_on_screen() {
    let account = AccountKey { network: Network::Discord, account: "acct".to_owned() };
    let began = Instant::now();
    let chatd = Daemon::start(&[], &[("TINYCHAT_TEST_KEYCHAIN_MS", "1500")], |store| {
        store.set_account(&account, "Robin", "me").unwrap();
        store.upsert_conversation(&account, &ConversationRow::new("dm", ConversationKind::Direct, "Mara")).unwrap();
    });
    // Answered at once, though no session has been read.
    assert_eq!(chatd.ask(Request::Ping), Response::Pong);
    assert!(began.elapsed() < Duration::from_secs(3), "the socket waited on the keychain: {:?}", began.elapsed());
    let (accounts, keychain, seq, started) = chatd.status();
    assert!(keychain.reading && keychain.denied.is_empty(), "{keychain:?}");
    assert!(started > 0);
    // The account whose session is not read yet is shown, offline, not dropped.
    let cached = accounts.iter().find(|status| status.key == account).expect("the cached account is listed");
    assert_eq!(cached.link, Link::Offline);

    // A read held up past a second is a prompt on screen: the shell is told, and the status says
    // which network it waits on.
    let told = chatd.changes(seq, Duration::from_millis(1_400));
    assert!(told.contains(&Change::Status), "{told:?}");
    let (_, keychain, ..) = chatd.status();
    assert!(keychain.waiting.is_some(), "{keychain:?}");
    if keychain.waiting == Some(Network::Discord) {
        let (accounts, ..) = chatd.status();
        let cached = accounts.iter().find(|status| status.key == account).unwrap();
        assert_eq!(cached.detail, "Waiting for permission to read your sign-in from the keychain.");
    }
    // Requests are served while it waits.
    assert!(matches!(chatd.ask(Request::Inbox { network: None, filter: InboxFilter::All, offset: 0, limit: 10 }), Response::Conversations { .. }));

    // Every network read: settled, and the same chatd (its start time has not moved).
    let settled = chatd.until("every session to be read", || {
        let (_, keychain, _, again) = chatd.status();
        keychain.is_settled().then_some(again)
    });
    assert_eq!(settled, started);
}

#[test]
fn a_request_that_panics_is_answered_and_what_it_held_is_taken_again() {
    let account = AccountKey { network: Network::Slack, account: "T1".to_owned() };
    let chatd = Daemon::start(&[], &[("TINYCHAT_TEST_PANIC", "boom")], |store| {
        store.set_account(&account, "Acme", "U1").unwrap();
    });
    // One connection: the panicking request is answered in words, and the next request on the
    // same connection is served, though the panic poisoned the store's lock.
    let mut stream = ChatStream::connect(&chatd.dir.join("chat.sock")).unwrap();
    write_frame(&mut stream, &Request::Find { network: None, query: "boom".to_owned(), limit: 5 }).unwrap();
    let answer: Response = read_frame(&mut stream).unwrap();
    assert!(matches!(answer, Response::Error { code: ErrorCode::Internal, .. }), "{answer:?}");
    write_frame(&mut stream, &Request::Status).unwrap();
    let answer: Response = read_frame(&mut stream).unwrap();
    assert!(matches!(&answer, Response::Status { accounts, .. } if accounts.iter().any(|status| status.key == account)), "{answer:?}");
    // And another connection, and another request that needs the store.
    assert!(matches!(chatd.ask(Request::Inbox { network: None, filter: InboxFilter::All, offset: 0, limit: 5 }), Response::Conversations { .. }));
    assert!(matches!(chatd.ask(Request::Find { network: None, query: "acme".to_owned(), limit: 5 }), Response::Conversations { .. }));
    // The panic is in the crash log, named.
    let log = std::fs::read_to_string(chatd.dir.join(protocol::crash::CRASH_LOG)).unwrap();
    assert!(log.contains("chatd panicked") && log.contains("a test asked this request to panic"), "{log}");
}

#[test]
fn a_second_chatd_for_the_same_directory_leaves_the_first_one_serving() {
    let chatd = Daemon::start(&[], &[], |_| {});
    let started = chatd.status().3;
    let second = Command::new(env!("CARGO_BIN_EXE_chatd")).env("TINYCHAT_DIR", &chatd.dir).output().unwrap();
    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"), "{second:?}");
    assert_eq!(chatd.status().3, started, "the first one still answers");
}

#[test]
fn a_frame_chatd_cannot_read_is_answered_in_words_and_the_connection_goes_on() {
    let chatd = Daemon::start(&[], &[], |_| {});
    let mut stream = ChatStream::connect(&chatd.dir.join("chat.sock")).unwrap();
    write_frame(&mut stream, &serde_json::json!({"op": "something_from_a_newer_shell"})).unwrap();
    let answer: Response = read_frame(&mut stream).unwrap();
    assert!(matches!(&answer, Response::Error { code: ErrorCode::Unsupported, detail } if detail.contains("versions")), "{answer:?}");
    write_frame(&mut stream, &Request::Ping).unwrap();
    assert_eq!(read_frame::<Response>(&mut stream).unwrap(), Response::Pong);
}

#[test]
fn a_network_without_spaces_is_one_list_with_its_pins_folders_archive_and_forum() {
    let chatd = Daemon::demo(&[]);
    let telegram = |id: &str| at(Network::Telegram, TELEGRAM, id);
    let home = |folder: Option<&str>, shelf| {
        chatd.rows(Request::Home { network: Network::Telegram, account: None, folder: folder.map(str::to_owned), shelf, offset: 0, limit: 50 })
    };
    let (rows, counts) = home(None, Shelf::Main);
    // Telegram's pins first, in their order, then by recency; no Space anywhere.
    assert_eq!(titles(&rows[..2]), ["Saved Messages", "Ines Marchetti"]);
    assert!(rows.iter().all(|row| row.space.is_none() && !row.archived && row.parent.is_none()));
    assert!(counts.unread > 0);
    let saved = &rows[0];
    assert_eq!((saved.special, saved.pin), (Special::Saved, Some(0)));
    // ✓✓ where the other side read our last message, ✓ where not; who spoke last in a group.
    let row = |title: &str| rows.iter().find(|row| row.title == title).unwrap_or_else(|| panic!("{title}"));
    assert!(row("Mara Okafor").preview_mine && row("Mara Okafor").seen);
    assert!(row("Lena Brandt").preview_mine && !row("Lena Brandt").seen);
    assert_eq!(row("Foraging Lisbon").preview_from, "Ines Marchetti");
    assert_eq!((row("Telegram").special, row("Telegram").mark, row("Telegram").read_only), (Special::Service, Some(Mark::Verified), true));
    assert!(row("Weather Bot").bot);
    assert_eq!(row("Crypto Giveaway Official").mark, Some(Mark::Scam));

    // Folders filter: a chat in two folders is in both; a folder's own pin leads it.
    let Response::Folders { folders } = chatd.ask(Request::Folders { network: Network::Telegram }) else { panic!("no folders") };
    let names: Vec<&str> = folders.iter().map(|folder| folder.name.as_str()).collect();
    assert_eq!(names, ["Work", "Family", "Channels"]);
    assert!(folders[0].unread >= 2, "{folders:?}");
    let (family, _) = home(Some("family"), Shelf::Main);
    assert_eq!(titles(&family), ["Family", "Grandma Rosa"]);
    let (work, _) = home(Some("work"), Shelf::Main);
    assert!(titles(&work).contains(&"Ines Marchetti") && titles(&work).contains(&"Mycology Forum"));

    // The archive is its own shelf, and out of the Inbox; channels stay out unless starred.
    let (archive, _) = home(None, Shelf::Archive);
    assert_eq!(titles(&archive), ["Trail Conditions", "Old Flatshare"]);
    let (inbox, _) = chatd.rows(Request::Inbox { network: Some(Network::Telegram), filter: InboxFilter::All, offset: 0, limit: 50 });
    assert!(inbox.iter().all(|row| !row.archived && row.kind != ConversationKind::Announcement), "{:?}", titles(&inbox));
    assert!(titles(&inbox).contains(&"Telegram"), "the login codes are in the Inbox");

    // A channel's sponsored message sits at its foot and is never its preview.
    assert!(chatd.row(&telegram("c9001")).preview.starts_with("Snow line"));
    // A forum's topics: its pin first, then by recency.
    let (topics, _) = chatd.rows(Request::Topics { forum: telegram("c9100"), offset: 0, limit: 10 });
    assert_eq!(titles(&topics), ["General", "ID requests", "Events"]);
}

#[test]
fn the_inbox_folds_apps_keeps_out_requests_and_the_quiet_and_the_home_lists_follow_their_network() {
    let chatd = Daemon::demo(&[]);
    let (inbox, counts) = chatd.rows(Request::Inbox { network: None, filter: InboxFilter::All, offset: 0, limit: 200 });
    // Apps are in it, flagged for the one Apps row, and raise only its dot.
    let apps: Vec<&str> = inbox.iter().filter(|row| row.bot).map(|row| row.title.as_str()).collect();
    for app in ["Lichen Bot", "Deploy Bot", "GitHub", "Workflows", "Weather Bot"] {
        assert!(apps.contains(&app), "{app} not among {apps:?}");
    }
    assert!(counts.apps >= 4, "{counts:?}");
    // Never: a message request, a hidden chat, a conversation with nothing in it, one quiet for
    // 40 days.
    for absent in ["Casimir Thorne", "Old project", "Wren Halloran", "Vendor sync"] {
        assert!(!titles(&inbox).contains(&absent), "{absent} is in the inbox");
    }
    let (requests, _) = chatd.rows(Request::Home { network: Network::Discord, account: None, folder: None, shelf: Shelf::Requests, offset: 0, limit: 10 });
    assert_eq!(titles(&requests), ["Casimir Thorne"]);

    // Teams' chats are its home's: Notes, meetings, apps; hidden never.
    let (chats, _) = chatd.rows(Request::Home { network: Network::Teams, account: None, folder: None, shelf: Shelf::Main, offset: 0, limit: 50 });
    let chat = |title: &str| chats.iter().find(|row| row.title == title).unwrap_or_else(|| panic!("{title}"));
    assert_eq!(chat("Notes").special, Special::Saved);
    assert!(chat("Q3 planning").meeting && chat("Sprint review").meeting && chat("Workflows").bot);
    assert!(!titles(&chats).contains(&"Old project"));
    assert!(chat("Ben Okoro").seen);
    // Two rows have no preview: on screen, the network is asked for them, once.
    let blank: Vec<Key> = chats.iter().filter(|row| row.preview.is_empty()).map(|row| row.key.clone()).collect();
    assert_eq!(blank.len(), 2, "{:?}", titles(&chats));
    chatd.ask(Request::Watch(Watching { rows: blank.clone(), ..Watching::default() }));
    chatd.until("the previews to be filled", || blank.iter().all(|key| !chatd.row(key).preview.is_empty()).then_some(()));

    // A team's hidden channels are in its tree, marked; a Slack section can be unread-only and
    // its quiet channels say so.
    let Response::Tree { conversations, .. } = chatd.ask(Request::Tree { space: at(Network::Teams, TEAMS, "19:team-engineering"), offset: 0, limit: 50, peek: true }) else {
        panic!("no tree")
    };
    let hidden: Vec<&str> = conversations.iter().filter(|row| row.opted_out).map(|row| row.title.as_str()).collect();
    assert_eq!(hidden, ["Archive 2024", "Hackathon"]);
    let Response::Tree { groups, conversations, .. } = chatd.ask(Request::Tree { space: at(Network::Slack, "T04HARBOR1", "T04HARBOR1"), offset: 0, limit: 100, peek: true }) else {
        panic!("no tree")
    };
    assert!(groups.iter().any(|group| group.name == "Product" && group.unread_only));
    let quiet: Vec<&str> = conversations.iter().filter(|row| row.quiet).map(|row| row.title.as_str()).collect();
    assert!(quiet.contains(&"offsite-2025") && quiet.contains(&"old-launch") && !quiet.contains(&"eng-platform"), "{quiet:?}");

    // Discord's server folders, and its In voice.
    let Response::Folders { folders } = chatd.ask(Request::Folders { network: Network::Discord }) else { panic!("no folders") };
    assert_eq!(folders.len(), 3);
    assert_eq!((folders[0].name.as_str(), folders[0].color, folders[0].spaces.len()), ("Nature", Some(0x3b_a5_5c), 2));
    let Response::Spaces { spaces } = chatd.ask(Request::Spaces { network: Some(Network::Discord) }) else { panic!("no spaces") };
    assert_eq!(spaces.iter().find(|space| space.name == "Moss & Lichen").and_then(|space| space.folder.as_deref()), Some("folder-nature"));
    assert!(spaces.iter().any(|space| space.name == "Synth DIY" && space.muted));
    let (voice, _) = chatd.rows(Request::InVoice { network: Some(Network::Discord) });
    assert!(titles(&voice).contains(&"Campfire"), "{:?}", titles(&voice));
}

#[test]
fn who_is_around_leaves_out_bots_and_says_what_people_are_doing() {
    let chatd = Daemon::demo(&[]);
    let Response::Around { requests, in_call, online, recent } = chatd.ask(Request::Around { network: None }) else {
        panic!("not around")
    };
    assert_eq!(requests, 1, "Dorian's friend request");
    let named = |people: &[protocol::Person], name: &str| people.iter().find(|person| person.name == name).cloned();
    let ines = named(&in_call, "Ines Marchetti").expect("Ines is in voice");
    assert_eq!(ines.activity.map(|activity| (activity.kind, activity.name)), Some((ActivityKind::InVoice, "Campfire".to_owned())));
    let ana = named(&in_call, "Ana Ruiz").expect("Ana is in a meeting");
    assert_eq!((ana.presence, ana.title.as_str()), (protocol::Presence::Busy, "Engineering manager"));
    let haruto = named(&online, "Haruto Senda").or_else(|| named(&recent, "Haruto Senda"));
    assert!(haruto.is_some_and(|haruto| haruto.activity.is_some_and(|activity| activity.name == "Hades")));
    for section in [&in_call, &online, &recent] {
        assert!(section.iter().all(|person| !person.bot), "a bot among people");
    }
    // Bots are never in People either.
    let Response::People { people, .. } = chatd.ask(Request::People { network: None, relation: None, query: Some("bot".to_owned()), offset: 0, limit: 50 }) else {
        panic!("no people")
    };
    assert!(people.iter().all(|person| !person.bot), "{people:?}");
}

#[test]
fn a_card_and_an_info_pane_are_typed_and_a_mute_is_the_network_s() {
    let chatd = Daemon::demo(&[]);
    let harbor = |id: &str| at(Network::Slack, "T04HARBOR1", id);
    let Response::Profile { profile } = chatd.ask(Request::Profile { person: harbor("U04MEMBER001"), space: None }) else { panic!("no card") };
    assert_eq!(profile.title, "Engineering manager");
    let phone = profile.contacts.iter().find(|contact| contact.kind == ContactKind::Phone).expect("a phone");
    assert!(phone.masked);
    assert!(profile.fields.is_empty(), "nothing left untyped: {:?}", profile.fields);
    // Opened from a server: what is said of them there.
    let ines = at(Network::Discord, ROBIN, "220000000000000001");
    let moss = at(Network::Discord, ROBIN, "300000000000000001");
    let Response::Profile { profile } = chatd.ask(Request::Profile { person: ines.clone(), space: Some(moss.clone()) }) else { panic!("no card") };
    assert!(!profile.nickname.is_empty() && !profile.roles.is_empty() && profile.joined.is_some(), "{profile:?}");
    let Response::Profile { profile } = chatd.ask(Request::Profile { person: ines, space: None }) else { panic!("no card") };
    assert!(profile.nickname.is_empty() && profile.friends_since.is_some());

    let showcase = at(Network::Discord, ROBIN, "400000000000000007");
    let Response::ConversationInfo { info } = chatd.ask(Request::ConversationInfo { conversation: showcase.clone() }) else { panic!("no info") };
    assert_eq!((info.notify, info.conversation.title.as_str()), (Notify::All, "showcase"));
    assert!(info.members.is_some_and(|members| members > 2));
    assert!(matches!(chatd.ask(Request::SetMute { conversation: showcase.clone(), notify: Notify::Nothing }), Response::Ok));
    assert!(chatd.row(&showcase).muted);
    let Response::ConversationInfo { info } = chatd.ask(Request::ConversationInfo { conversation: showcase }) else { panic!("no info") };
    assert_eq!(info.notify, Notify::Nothing, "a mute is not answered from what was kept before it");
    let Response::SpaceInfo { info } = chatd.ask(Request::SpaceInfo { space: moss }) else { panic!("no space info") };
    assert!(info.members.is_some() && !info.roles.is_empty());

    // What was shared: from the cache, said to be partial; searched on request, complete.
    let shared = |search| Request::Shared { conversation: at(Network::Discord, ROBIN, "400000000000000007"), kind: protocol::SharedKind::Media, before: None, limit: 20, search };
    assert!(matches!(chatd.ask(shared(false)), Response::Shared { complete: false, .. }));
    assert!(matches!(chatd.ask(shared(true)), Response::Shared { complete: true, .. }));
}

#[test]
fn files_go_from_their_paths_with_progress_within_the_account_s_limits_and_can_be_cancelled() {
    let chatd = Daemon::demo(&[("TINYCHAT_DEMO_UPLOAD_MS", "150")]);
    let telegram = at(Network::Telegram, TELEGRAM, "u7000001");
    let photo = chatd.dir.join("trail.png");
    std::fs::write(&photo, vec![7u8; 900 * 1024]).unwrap();
    let with = |path: &std::path::Path| Outgoing {
        files: vec![OutgoingFile { path: path.to_string_lossy().into_owned(), name: String::new(), spoiler: true, as_file: false, original: false }],
        ..Outgoing::default()
    };
    let send = |client_id: &str, with: Outgoing| {
        chatd.ask(Request::Send { conversation: telegram.clone(), client_id: client_id.to_owned(), text: "the trail".to_owned(), reply_to: None, with })
    };
    let seq = chatd.status().2;
    let Response::Sent { message } = send("1700000000000001", with(&photo)) else { panic!("not sent") };
    // The pending message lists the file from where it is.
    assert_eq!(message.state, SendState::Pending);
    assert_eq!((message.attachments[0].name.as_str(), message.attachments[0].spoiler), ("trail.png", true));
    assert_eq!(message.attachments[0].file.path.as_deref(), Some(photo.to_string_lossy().as_ref()));
    let sent = chatd.until("the upload to finish", || chatd.message(&telegram, "1700000000000001").filter(|message| message.state == SendState::Sent));
    assert_eq!(sent.attachments.len(), 1);
    let progress: Vec<(u64, u64)> = chatd
        .changes(seq, Duration::from_millis(200))
        .into_iter()
        .filter_map(|change| match change {
            Change::Upload { client_id, sent, total, .. } if client_id == "1700000000000001" => Some((sent, total)),
            _ => None,
        })
        .collect();
    // Told as one entry per message, replaced as it goes: the last word is the whole file.
    assert_eq!(progress.last(), Some(&(900 * 1024, 900 * 1024)), "{progress:?}");

    // Over the account's limit: refused on the spot, naming it; nothing is stored.
    let big = chatd.dir.join("big.mov");
    std::fs::File::create(&big).unwrap().set_len(11 << 20).unwrap();
    let refused = chatd.ask(Request::Send { conversation: at(Network::Discord, ROBIN, "500000000000000001"), client_id: "1700000000000002".to_owned(), text: String::new(), reply_to: None, with: with(&big) });
    assert!(matches!(&refused, Response::Error { code: ErrorCode::Refused, detail } if detail.contains("too big for Discord (limit 10 MB)")), "{refused:?}");
    assert!(chatd.message(&at(Network::Discord, ROBIN, "500000000000000001"), "1700000000000002").is_none());

    // Cancelled on its way: failed, in words, and Retry sends the file again.
    let Response::Sent { .. } = send("1700000000000003", with(&photo)) else { panic!("not sent") };
    assert!(matches!(chatd.ask(Request::CancelSend { conversation: telegram.clone(), client_id: "1700000000000003".to_owned() }), Response::Ok));
    let failed = chatd.until("the cancel", || chatd.message(&telegram, "1700000000000003").filter(|message| message.state != SendState::Pending));
    assert_eq!(failed.state, SendState::Failed { reason: "Cancelled".to_owned() });
    let Response::Sent { message } = chatd.ask(Request::Retry { conversation: telegram.clone(), client_id: "1700000000000003".to_owned() }) else {
        panic!("not retried")
    };
    assert_eq!(message.attachments.len(), 1);
    chatd.until("the retry to go", || chatd.message(&telegram, "1700000000000003").filter(|message| message.state == SendState::Sent));
}

#[test]
fn the_pickers_are_answered_on_demand_by_page_and_what_an_account_may_send_is_in_its_status() {
    let chatd = Daemon::demo(&[]);
    let telegram = AccountKey { network: Network::Telegram, account: TELEGRAM.to_owned() };
    let catalog = |account: &AccountKey, kind, query: &str, cursor: Option<String>| match chatd.ask(Request::Catalog { account: account.clone(), kind, space: None, query: query.to_owned(), cursor }) {
        Response::Catalog { items, cursor } => (items, cursor),
        other => panic!("not a catalog: {other:?}"),
    };
    let (cats, _) = catalog(&telegram, CatalogKind::Gifs, "cat", None);
    assert_eq!(cats.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["cat typing", "cat nap"]);
    assert!(cats.iter().all(|item| item.animated && item.file.is_some()));
    let (first, cursor) = catalog(&telegram, CatalogKind::Gifs, "", None);
    let (rest, end) = catalog(&telegram, CatalogKind::Gifs, "", cursor);
    assert_eq!((first.len(), rest.len(), end), (8, 4, None));
    let discord = AccountKey { network: Network::Discord, account: ROBIN.to_owned() };
    let (emoji, _) = catalog(&discord, CatalogKind::Emoji, "", None);
    assert!(emoji.iter().any(|item| item.name == ":mossparrot:"));
    // A network without stickers has none.
    let teams = AccountKey { network: Network::Teams, account: TEAMS.to_owned() };
    assert!(catalog(&teams, CatalogKind::Stickers, "", None).0.is_empty());

    let (accounts, ..) = chatd.status();
    let can = |network| accounts.iter().find(|status| status.key.network == network).map(|status| status.can).unwrap();
    assert_eq!(can(Network::Discord).max_file_bytes, 10 << 20);
    assert!(can(Network::Telegram).as_file && can(Network::Telegram).silent);
    assert!(!can(Network::Teams).gifs && can(Network::Teams).max_file_bytes > 0);
}
