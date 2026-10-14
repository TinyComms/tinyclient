//! Runs the real chatd and mediad binaries and talks to chatd over its local socket.

use chatd::store::{ConversationRow, SpaceRow, Store};
use protocol::{
    read_frame, write_frame, AccountKey, CallFailure, CallState, CallSupport, Change, Content,
    Conversation, ConversationKind, Direction, ErrorCode, Group, GroupKind, InboxFilter, Key, Link,
    Media, MediaFile, MediaKind, MediaRequest, Message, Network, Page, Presence, RelateAction,
    Relation, Request, Response, SendState, OCCUPANTS_LIMIT,
};
use protocol::ChatStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

fn child_pids(parent: u32, name: &str) -> Vec<u32> {
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name = '{name}.exe'\" | Where-Object {{ $_.ParentProcessId -eq {parent} }}).ProcessId"
        );
        let out = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
        return String::from_utf8(out.stdout).unwrap().lines().filter_map(|line| line.trim().parse().ok()).collect();
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("pgrep").args(["-P", &parent.to_string(), name]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().lines().filter_map(|pid| pid.trim().parse().ok()).collect()
    }
}

fn process_rss_kb(pid: u32) -> u64 {
    #[cfg(windows)]
    {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("(Get-Process -Id {pid}).WorkingSet64")])
            .output()
            .expect("working set");
        let bytes: u64 = String::from_utf8(out.stdout).unwrap().trim().parse().unwrap();
        return bytes / 1024;
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
    }
}
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Once;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// chatd's idle budget for all four networks. Nothing is connected yet, so today this only
/// catches the chat process picking up weight it should not have.
const IDLE_BUDGET_KB: u64 = 250 * 1024;

/// mediad is another package's binary, so cargo does not build it for these tests.
fn mediad() -> PathBuf {
    let chatd = Path::new(env!("CARGO_BIN_EXE_chatd"));
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut build = Command::new(cargo);
        build.args(["build", "-p", "mediad", "--bin", "mediad"]);
        if chatd.parent().is_some_and(|dir| dir.ends_with("release")) {
            build.arg("--release");
        }
        assert!(build.status().unwrap().success(), "could not build mediad");
    });
    chatd.with_file_name(format!("mediad{}", std::env::consts::EXE_SUFFIX))
}

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(seed: impl FnOnce(&mut Store)) -> Daemon {
        Daemon::with(&[], &[], seed)
    }

    /// The demo: three connected accounts whose "network" is chatd's own demo adapter, which
    /// does what a network would when asked. `env` sets how it behaves.
    fn demo(env: &[(&str, &str)]) -> Daemon {
        Daemon::with(&["--seed-demo"], env, |_| {})
    }

    fn with(args: &[&str], env: &[(&str, &str)], seed: impl FnOnce(&mut Store)) -> Daemon {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Short path: a unix socket address holds about 100 bytes.
        let dir = PathBuf::from(format!(
            "/tmp/tinyclient-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        seed(&mut Store::open(&dir.join("chat.sqlite")).unwrap());

        let child = Command::new(env!("CARGO_BIN_EXE_chatd"))
            .args(args)
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_MEDIAD", mediad())
            // No test call opens a sound device.
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
            .envs(env.iter().copied())
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

    fn ask(&self, request: Request) -> Response {
        let mut stream = ChatStream::connect(&self.dir.join("chat.sock")).unwrap();
        write_frame(&mut stream, &request).unwrap();
        read_frame(&mut stream).unwrap()
    }

    /// Pids of the mediad processes chatd has spawned, from the process table.
    fn mediad_pids(&self) -> Vec<u32> {
        child_pids(self.child.id(), "mediad")
    }

    /// Asks until the answer is the one wanted, and gives it.
    fn until(&self, what: &str, request: Request, wanted: impl Fn(&Response) -> bool) -> Response {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let answer = self.ask(request.clone());
            if wanted(&answer) {
                return answer;
            }
            assert!(Instant::now() < deadline, "{what} did not happen: {answer:?}");
            sleep(Duration::from_millis(20));
        }
    }

    fn seq(&self) -> u64 {
        let Response::Status { seq, .. } = self.ask(Request::Status) else {
            panic!("not a status");
        };
        seq
    }

    /// Every change the shell is told of from `since` for `time`, in order.
    fn changes(&self, mut since: u64, time: Duration) -> Vec<Change> {
        let socket = self.dir.join("chat.sock");
        let deadline = Instant::now() + time;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let mut stream = ChatStream::connect(&socket).unwrap();
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

    fn row(&self, conversation: &Key) -> Conversation {
        match self.ask(Request::Conversation { conversation: conversation.clone() }) {
            Response::Conversation { conversation } => conversation,
            other => panic!("not a row: {other:?}"),
        }
    }

    fn newest(&self, conversation: &Key) -> Vec<Message> {
        let request = Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 200 };
        match self.ask(request) {
            Response::Messages { messages, .. } => messages,
            other => panic!("not messages: {other:?}"),
        }
    }

    fn rss_kb(&self) -> u64 {
        process_rss_kb(self.child.id())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

fn gone_within(pid: u32, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while running(pid) {
        if Instant::now() > deadline {
            return false;
        }
        sleep(Duration::from_millis(10));
    }
    true
}

/// Every seeded conversation belongs to the one account of its network.
fn key(network: Network, id: &str) -> Key {
    Key {
        network,
        account: "acct".to_owned(),
        id: id.to_owned(),
    }
}

/// One live incoming message, in a channel the adapter listed under `title`.
fn incoming(store: &mut Store, title: &str, network: Network, conversation_id: &str, id: &str, body: &str) {
    let conversation = key(network, conversation_id);
    let row = ConversationRow {
        listed: true,
        ..ConversationRow::new(conversation_id, ConversationKind::Channel, title)
    };
    store.upsert_conversation(&conversation.account(), &row).unwrap();
    // Recent, so retention leaves it alone; a longer id is a later message.
    let hour_ago = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 3_600;
    let message = Message::plain(id, "mara", body, hour_ago + id.len() as i64, false);
    store.ingest(&conversation, &message, true).unwrap();
}

fn messages(network: Network, conversation_id: &str, limit: u32) -> Request {
    Request::Messages {
        conversation: key(network, conversation_id),
        page: Page::Newest,
        limit,
    }
}

/// What a shell does on opening a conversation: reads the newest messages and, only when the
/// user is looking, says how far they read.
fn open(chatd: &Daemon, network: Network, conversation_id: &str, read: bool) -> Response {
    let opened = chatd.ask(messages(network, conversation_id, 50));
    if let (true, Response::Messages { messages, .. }) = (read, &opened) {
        if let Some(newest) = messages.last() {
            let marked = chatd.ask(Request::MarkRead {
                conversation: key(network, conversation_id),
                message_id: newest.id.clone(),
            });
            assert_eq!(marked, Response::Ok);
        }
    }
    opened
}

fn call_start() -> Request {
    Request::CallStart {
        conversation: key(Network::Teams, "19:standup"),
    }
}

/// A conversation and what pressing its call button does.
fn callable(store: &mut Store, id: &str, call: CallSupport) {
    let row = ConversationRow {
        call,
        ..ConversationRow::new(id, ConversationKind::Group, id)
    };
    store.upsert_conversation(&key(Network::Teams, id).account(), &row).unwrap();
}

/// The meeting the call tests join: one tinyclient carries itself.
fn standup(store: &mut Store) {
    callable(store, "19:standup", CallSupport::Native);
}

fn failed_with(answer: &Response, reason: CallFailure) -> bool {
    matches!(answer, Response::Status { call: Some(call), .. }
        if matches!(&call.state, CallState::Failed { reason: why, detail } if *why == reason && !detail.is_empty()))
}

/// The demo's Discord account and things in it.
const ROBIN: &str = "210000000000000000";
const MOSS: &str = "300000000000000001";
const SHOWCASE: &str = "400000000000000007";
const CAMPFIRE: &str = "400000000000000035";

fn demo(id: &str) -> Key {
    Key {
        network: Network::Discord,
        account: ROBIN.to_owned(),
        id: id.to_owned(),
    }
}

fn harbor(id: &str) -> Key {
    Key {
        network: Network::Slack,
        account: "T04HARBOR1".to_owned(),
        id: id.to_owned(),
    }
}

/// The width a PNG file says it has.
fn png_width(path: &str) -> u32 {
    let bytes = std::fs::read(path).unwrap();
    u32::from_be_bytes(bytes[16..20].try_into().unwrap())
}

#[test]
fn idle_chatd_reports_four_networks_and_runs_no_mediad() {
    let chatd = Daemon::start(|_| {});

    let Response::Status { networks, media, call, .. } = chatd.ask(Request::Status) else {
        panic!("not a status");
    };
    let seen: Vec<_> = networks.iter().map(|n| (n.network, n.link)).collect();
    // A scratch directory never reads the keychain, so nothing is signed in.
    assert_eq!(seen, Network::ALL.map(|network| (network, Link::SignedOut)));
    assert_eq!(media, Media::Stopped);
    assert_eq!(call, None);

    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new());
    let rss = chatd.rss_kb();
    assert!(rss < IDLE_BUDGET_KB, "idle chatd is {rss} KB");
}

#[test]
fn opening_one_conversation_returns_its_messages_and_clears_only_its_unread() {
    // The same conversation id on two networks: they must stay separate.
    let chatd = Daemon::start(|store| {
        incoming(store, "#platform", Network::Slack, "C01", "a", "deploy is green");
        incoming(store, "#platform", Network::Slack, "C01", "bb", "rolling at 3");
        incoming(store, "Release", Network::Teams, "C01", "c", "share at 2pm?");
    });

    let unread = |chatd: &Daemon| {
        let inbox = Request::Inbox {
            network: None,
            filter: InboxFilter::All,
            offset: 0,
            limit: 50,
        };
        let Response::Conversations { conversations, total, .. } = chatd.ask(inbox) else {
            panic!("not a list");
        };
        assert_eq!(total as usize, conversations.len());
        let mut rows: Vec<_> = conversations
            .into_iter()
            .map(|c| (c.key.network, c.key.id, c.title, c.unread, c.preview))
            .collect();
        rows.sort_by_key(|row| row.0 as u8);
        rows
    };
    assert_eq!(
        unread(&chatd),
        [
            (Network::Slack, "C01".into(), "#platform".into(), 2, "rolling at 3".into()),
            (Network::Teams, "C01".into(), "Release".into(), 1, "share at 2pm?".into()),
        ]
    );

    // A shell in the background shows the messages without claiming the user read them.
    let before = unread(&chatd);
    open(&chatd, Network::Slack, "C01", false);
    assert_eq!(unread(&chatd), before);

    let opened = open(&chatd, Network::Slack, "C01", true);
    let Response::Messages { conversation, messages, last_read, .. } = opened else {
        panic!("not messages: {opened:?}");
    };
    assert_eq!(conversation, key(Network::Slack, "C01"));
    // Where the "New" divider goes is from before this open marked anything read.
    assert_eq!(last_read, None);
    let bodies: Vec<_> = messages
        .iter()
        .map(|m| (conversation.network, conversation.id.as_str(), m.text.as_str()))
        .collect();
    assert_eq!(
        bodies,
        [
            (Network::Slack, "C01", "deploy is green"),
            (Network::Slack, "C01", "rolling at 3"),
        ]
    );

    let after = unread(&chatd);
    assert_eq!((after[0].0, after[0].3), (Network::Slack, 0));
    assert_eq!((after[1].0, after[1].3), (Network::Teams, 1));

    let missing = open(&chatd, Network::Discord, "C01", true);
    assert!(matches!(missing, Response::Error { code: ErrorCode::NotFound, .. }), "{missing:?}");
}

#[test]
fn a_waiting_shell_is_woken_by_a_change_and_not_before() {
    let chatd = Daemon::start(|store| {
        incoming(store, "#platform", Network::Slack, "C01", "a", "deploy is green");
    });
    // The sessions are read after the socket answers, and that is told as it happens: start
    // from after it.
    let seq = chatd.until("the sessions to be read", Request::Status, |answer| matches!(answer, Response::Status { keychain, .. } if keychain.is_settled()));
    let Response::Status { seq, .. } = seq else {
        panic!("not a status");
    };

    let socket = chatd.dir.join("chat.sock");
    let waiter = std::thread::spawn(move || {
        let mut stream = ChatStream::connect(&socket).unwrap();
        write_frame(&mut stream, &Request::Wait { since: seq }).unwrap();
        let woken: Response = read_frame(&mut stream).unwrap();
        (woken, Instant::now())
    });
    sleep(Duration::from_millis(300));
    assert!(!waiter.is_finished(), "wait answered with nothing changed");

    // Reading the conversation changes its unread count, which is a change the shell draws.
    let changed_at = Instant::now();
    open(&chatd, Network::Slack, "C01", true);
    let (woken, woken_at) = waiter.join().unwrap();
    let Response::Changes { seq: after, reset, changes } = woken else {
        panic!("not changes: {woken:?}");
    };
    assert!(after > seq, "seq stayed at {seq}");
    assert!(woken_at - changed_at < Duration::from_secs(1), "woken late");
    // The shell is told which row to read again, not to read everything.
    assert!(!reset);
    let conversation = key(Network::Slack, "C01");
    assert!(changes.contains(&Change::Conversation { conversation }), "{changes:?}");

    // A shell that outlived a restart, or fell too far behind, is told to start over at once.
    let stale = chatd.ask(Request::Wait { since: 7 });
    assert!(matches!(stale, Response::Changes { reset: true, .. }), "{stale:?}");
}

#[test]
fn sending_on_an_offline_network_is_refused_and_stores_nothing() {
    let chatd = Daemon::start(|store| {
        incoming(store, "#platform", Network::Slack, "C01", "a", "deploy is green");
    });

    let sent = chatd.ask(Request::Send {
        conversation: key(Network::Slack, "C01"),
        client_id: "draft-1".into(),
        text: "on it".into(),
        reply_to: None, with: Default::default(),
    });
    assert!(matches!(sent, Response::Error { code: ErrorCode::Offline, .. }), "{sent:?}");

    let Response::Messages { messages, .. } = open(&chatd, Network::Slack, "C01", true) else {
        panic!("not messages");
    };
    let bodies: Vec<_> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(bodies, ["deploy is green"]);
}

#[test]
fn a_call_spawns_mediad_and_ending_it_exits_mediad_while_chat_keeps_answering() {
    let chatd = Daemon::start(standup);
    let idle_rss = chatd.rss_kb();

    let started = chatd.ask(call_start());
    let Response::Status { media, call: Some(call), .. } = started else {
        panic!("call did not start: {started:?}");
    };
    assert_eq!(media, Media::Running);
    assert_eq!(call.conversation, key(Network::Teams, "19:standup"));
    let pids = chatd.mediad_pids();
    assert_eq!(pids.len(), 1, "one mediad per call");

    // A second call is refused rather than spawning a second mediad.
    let second = chatd.ask(call_start());
    assert!(matches!(second, Response::Error { code: ErrorCode::CallActive, .. }), "{second:?}");
    assert_eq!(chatd.mediad_pids(), pids);

    assert_eq!(chatd.ask(Request::Ping), Response::Pong);
    let in_call_rss = chatd.rss_kb();
    assert!(
        in_call_rss < idle_rss + 16 * 1024,
        "chatd grew from {idle_rss} KB to {in_call_rss} KB during a call"
    );

    let ended = chatd.ask(Request::CallEnd);
    assert!(
        matches!(ended, Response::Status { media: Media::Stopped, call: None, .. }),
        "{ended:?}"
    );
    assert!(!running(pids[0]), "mediad outlived the call");
    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new());
    assert_eq!(chatd.ask(Request::Ping), Response::Pong);
}

#[test]
fn a_call_starts_only_where_tinyclient_carries_calls_itself_and_a_refusal_spawns_nothing() {
    let chatd = Daemon::start(|store| {
        standup(store);
        callable(store, "huddle", CallSupport::Handoff);
        callable(store, "stage", CallSupport::None);
    });
    for id in ["huddle", "stage"] {
        let refused = chatd.ask(Request::CallStart { conversation: key(Network::Teams, id) });
        assert!(matches!(refused, Response::Error { code: ErrorCode::Unsupported, .. }), "{id}: {refused:?}");
    }
    let unknown = chatd.ask(Request::CallStart { conversation: key(Network::Teams, "nowhere") });
    assert!(matches!(unknown, Response::Error { code: ErrorCode::NotFound, .. }), "{unknown:?}");
    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new(), "a refused call must not start mediad");
    let idle = chatd.ask(Request::Status);
    assert!(matches!(idle, Response::Status { media: Media::Stopped, call: None, .. }), "{idle:?}");

    assert!(matches!(chatd.ask(call_start()), Response::Status { media: Media::Running, call: Some(_), .. }));
}

#[test]
fn mediad_dying_before_the_voice_session_arrives_is_a_failure_in_words_not_a_vanished_call() {
    let chatd = Daemon::start(standup);
    chatd.ask(call_start());
    let pid = chatd.mediad_pids()[0];

    assert!(Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    // The thread that reads mediad settles the call once its stdout closes.
    let settled = chatd.until("the failure", Request::Status, |status| failed_with(status, CallFailure::Engine));
    assert!(matches!(settled, Response::Status { media: Media::Stopped, .. }), "{settled:?}");
    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new());
    // It stays until dismissed, and "try again" replaces it with a new call.
    sleep(Duration::from_millis(200));
    assert!(failed_with(&chatd.ask(Request::Status), CallFailure::Engine));
    assert!(matches!(chatd.ask(call_start()), Response::Status { media: Media::Running, .. }));
}

#[test]
fn mediad_exits_when_chatd_is_killed_mid_call() {
    let mut chatd = Daemon::start(standup);
    chatd.ask(call_start());
    let pid = chatd.mediad_pids()[0];

    chatd.child.kill().unwrap();
    chatd.child.wait().unwrap();
    assert!(gone_within(pid, Duration::from_secs(5)), "mediad was orphaned");
}

#[test]
fn a_share_is_one_encode_and_one_subscribed_decode() {
    let chatd = Daemon::start(standup);

    let no_call = chatd.ask(Request::Media(MediaRequest::ShareSend { on: true }));
    assert!(matches!(no_call, Response::Error { code: ErrorCode::NoCall, .. }), "{no_call:?}");
    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new());

    chatd.ask(call_start());
    let watch = |source: &str| Request::Media(MediaRequest::ShareWatch { source: Some(source.into()) });
    let speaker = |source: &str| Request::Media(MediaRequest::Speaker { source: Some(source.into()) });
    chatd.ask(Request::Media(MediaRequest::ShareSend { on: true }));
    chatd.ask(watch("alice"));
    chatd.ask(speaker("carol"));
    chatd.ask(Request::Media(MediaRequest::ShareSend { on: true }));
    chatd.ask(speaker("dave"));
    let last = chatd.ask(watch("bob"));

    let Response::Status { call: Some(call), .. } = last else {
        panic!("no call: {last:?}");
    };
    let sources = |direction, content| -> Vec<&str> {
        call.video
            .iter()
            .filter(|s| s.direction == direction && s.content == content)
            .map(|s| s.source.as_str())
            .collect()
    };
    assert_eq!(sources(Direction::Encode, Content::Share), [""]);
    assert_eq!(sources(Direction::Decode, Content::Share), ["bob"]);
    assert_eq!(sources(Direction::Decode, Content::Speaker), ["dave"]);
    assert_eq!(call.video.len(), 3, "{:?}", call.video);
}

#[test]
fn lists_and_history_come_in_pages_that_say_how_much_more_there_is() {
    let chatd = Daemon::start(|store| {
        for (conversation, title) in [("C01", "#one"), ("C02", "#two"), ("C03", "#three")] {
            incoming(store, title, Network::Slack, conversation, "a", "hello");
        }
        for id in ["bb", "ccc", "dddd", "eeeee"] {
            incoming(store, "#one", Network::Slack, "C01", id, id);
        }
    });

    let inbox = |offset| {
        let request = Request::Inbox {
            network: Some(Network::Slack),
            filter: InboxFilter::Unread,
            offset,
            limit: 2,
        };
        let Response::Conversations { conversations, total, .. } = chatd.ask(request) else {
            panic!("not a list");
        };
        (conversations.into_iter().map(|c| c.title).collect::<Vec<_>>(), total)
    };
    // Newest first: #one has the latest message; the other two tie and fall back to their ids.
    assert_eq!(inbox(0), (vec!["#one".to_owned(), "#two".to_owned()], 3));
    assert_eq!(inbox(2), (vec!["#three".to_owned()], 3));

    let page = |page| {
        let request = Request::Messages {
            conversation: key(Network::Slack, "C01"),
            page,
            limit: 2,
        };
        let Response::Messages { messages, older, newer, .. } = chatd.ask(request) else {
            panic!("not messages");
        };
        (messages.into_iter().map(|m| m.text).collect::<Vec<_>>(), older, newer)
    };
    assert_eq!(page(Page::Newest), (vec!["dddd".to_owned(), "eeeee".to_owned()], true, false));
    let before = page(Page::Before { id: "dddd".into() });
    assert_eq!(before, (vec!["bb".to_owned(), "ccc".to_owned()], true, true));
    let oldest = page(Page::Before { id: "bb".into() });
    assert_eq!(oldest, (vec!["hello".to_owned()], false, true));

    // A limit past the cap is cut to it rather than answered with everything.
    let huge = chatd.ask(messages(Network::Slack, "C01", u32::MAX));
    assert!(matches!(huge, Response::Messages { ref messages, .. } if messages.len() == 5), "{huge:?}");
}

#[test]
fn what_only_a_network_can_answer_is_refused_in_words_and_the_cache_answers_the_rest() {
    let account = AccountKey {
        network: Network::Discord,
        account: "acct".to_owned(),
    };
    let seeded = account.clone();
    let chatd = Daemon::start(move |store| {
        incoming(store, "Mara", Network::Discord, "D1", "a", "hi");
        let icon = MediaFile {
            width: 80,
            height: 80,
            ..MediaFile::new("icon-1", MediaKind::Icon)
        };
        store.register_media(&seeded, &[(icon, "https://cdn.example/icon-1".into())]).unwrap();
        let space = SpaceRow {
            id: "G1".into(),
            name: "Rust".into(),
            icon: Some("icon-1".into()),
            position: 0,
        };
        store.replace_spaces(&seeded, &[space]).unwrap();
        let channel = ConversationRow::new("C9", ConversationKind::Channel, "#gpui");
        store.replace_tree(&key(Network::Discord, "G1"), &[], &[channel]).unwrap();
    });
    let conversation = key(Network::Discord, "D1");
    let unsupported = |request: Request| {
        let answer = chatd.ask(request.clone());
        assert!(
            matches!(answer, Response::Error { code: ErrorCode::Unsupported, .. }),
            "{request:?} answered {answer:?}"
        );
    };
    unsupported(Request::Members { conversation: conversation.clone(), offset: 0, limit: 100 });
    unsupported(Request::StartDirect { person: key(Network::Discord, "42") });
    unsupported(Request::Handoff { conversation: conversation.clone() });

    // The tree and its Space come from the cache, with the icon's size known before its bytes.
    let Response::Spaces { spaces } = chatd.ask(Request::Spaces { network: None }) else {
        panic!("not spaces");
    };
    let icon = spaces[0].icon.clone().expect("the space has an icon");
    assert_eq!((spaces[0].name.as_str(), icon.width, icon.path.as_deref()), ("Rust", 80, None));
    let tree = chatd.ask(Request::Tree { space: key(Network::Discord, "G1"), offset: 0, limit: 50, peek: false });
    let Response::Tree { conversations, total, .. } = tree else {
        panic!("not a tree: {tree:?}");
    };
    assert_eq!((conversations[0].title.as_str(), total), ("#gpui", 1));
    // A channel nobody mentioned me in is in its Space, not in the Inbox.
    let inbox = Request::Inbox { network: None, filter: InboxFilter::All, offset: 0, limit: 50 };
    let Response::Conversations { conversations, .. } = chatd.ask(inbox) else {
        panic!("not a list");
    };
    assert_eq!(conversations.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Mara"]);

    // Asking for a file names what is known; chatd holds no bytes and invents no path.
    let fetched = chatd.ask(Request::Fetch { retry: false, size: None, account, ids: vec!["icon-1".into(), "nope".into()] });
    assert_eq!(fetched, Response::Media { files: vec![icon] });

    // Typing to a network that is not connected is dropped quietly; it is not an error to type.
    assert_eq!(chatd.ask(Request::Typing { conversation }), Response::Ok);
    let missing = chatd.ask(Request::Tree { space: key(Network::Discord, "G2"), offset: 0, limit: 50, peek: false });
    assert!(matches!(missing, Response::Error { code: ErrorCode::NotFound, .. }), "{missing:?}");
}

#[test]
fn unread_and_mentions_are_different_lists_and_the_inbox_counts_each() {
    let chatd = Daemon::start(|store| {
        let account = key(Network::Slack, "").account();
        let hour_ago = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 3_600;
        let mut heard = |id: &str, kind, title: &str, mention: bool, muted: bool| {
            let row = ConversationRow {
                listed: true,
                ..ConversationRow::new(id, kind, title)
            };
            store.upsert_conversation(&account, &row).unwrap();
            let mut message = Message::plain("1", "mara", "hello", hour_ago, false);
            message.mentions_me = mention;
            store.ingest(&key(Network::Slack, id), &message, true).unwrap();
            store.set_muted(&key(Network::Slack, id), muted).unwrap();
        };
        heard("D1", ConversationKind::Direct, "Mara", false, false);
        heard("C1", ConversationKind::Channel, "#ops", true, false);
        heard("C2", ConversationKind::Channel, "#noise", false, false);
        heard("D2", ConversationKind::Direct, "Dev (muted)", true, true);
    });
    let inbox = |filter| {
        let request = Request::Inbox { network: None, filter, offset: 0, limit: 50 };
        let Response::Conversations { conversations, total, counts } = chatd.ask(request) else {
            panic!("not a list");
        };
        let mut titles: Vec<String> = conversations.into_iter().map(|row| row.title).collect();
        titles.sort();
        assert_eq!(titles.len() as u32, total);
        // The counts are the same whichever filter is on screen: they label the other tabs.
        assert_eq!((counts.all, counts.unread, counts.mentions), (4, 3, 2));
        titles
    };
    // Unread: whatever I have not seen and did not mute. Mentions: who is waiting on me, muted
    // or not. A direct message nobody mentioned me in is the first and not the second.
    assert_eq!(inbox(InboxFilter::Unread), ["#noise", "#ops", "Mara"]);
    assert_eq!(inbox(InboxFilter::Mentions), ["#ops", "Dev (muted)"]);
    assert_eq!(inbox(InboxFilter::Directs), ["Dev (muted)", "Mara"]);
    assert_eq!(inbox(InboxFilter::All).len(), 4);
}

#[test]
fn a_row_says_where_it_is_and_a_thread_lives_and_dies_with_its_parent() {
    let chatd = Daemon::start(|store| {
        let account = key(Network::Discord, "").account();
        let space = |id: &str, name: &str| SpaceRow { id: id.into(), name: name.into(), icon: None, position: 0 };
        store.replace_spaces(&account, &[space("G1", "Rustaceans"), space("G2", "Moss & Lichen")]).unwrap();
        for (space, channel, topic) in [("G1", "C1", "Borrow checker support group\nsecond line"), ("G2", "C2", "")] {
            let general = ConversationRow {
                group: Some("text".into()),
                topic: topic.into(),
                ..ConversationRow::new(channel, ConversationKind::Channel, "general")
            };
            store.replace_tree(&key(Network::Discord, space), &[Group::new("text", "Text channels", 0)], &[general]).unwrap();
        }
        let recent = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 60;
        store.ingest(&key(Network::Discord, "C1"), &Message::plain("100", "mara", "the first message", recent, false), false).unwrap();
        // One thread hangs off a message that is stored, one off a message that has aged out,
        // and one off a channel that is gone.
        for (id, parent, root) in [("T-kept", "C1", "100"), ("T-aged", "C1", "7"), ("T-orphan", "C-gone", "1")] {
            let thread = ConversationRow {
                space: Some("G1".into()),
                parent: Some(parent.into()),
                root: Some(root.into()),
                ..ConversationRow::new(id, ConversationKind::Thread, "a thread")
            };
            store.upsert_conversation(&account, &thread).unwrap();
            store.ingest(&key(Network::Discord, id), &Message::plain("1", "mara", "a reply", recent, false), false).unwrap();
        }
        // The network sends the tree again: a thread that names its parent is not the tree's
        // to list, and must survive it.
        let general = ConversationRow {
            group: Some("text".into()),
            topic: "Borrow checker support group\nsecond line".into(),
            ..ConversationRow::new("C1", ConversationKind::Channel, "general")
        };
        store.replace_tree(&key(Network::Discord, "G1"), &[Group::new("text", "Text channels", 0)], &[general]).unwrap();
    });

    let general = chatd.row(&key(Network::Discord, "C1"));
    assert_eq!((general.space_title.as_str(), general.group_title.as_str()), ("Rustaceans", "Text channels"));
    assert_eq!(general.subtitle(), "Rustaceans › Text channels · Borrow checker support group");
    // Two channels of one name are told apart by their Space, in one answer.
    let found = chatd.ask(Request::Find { network: None, query: "gener".into(), limit: 10 });
    let Response::Conversations { conversations, .. } = found else {
        panic!("not a list: {found:?}");
    };
    let mut places: Vec<(String, String)> = conversations.into_iter().map(|row| (row.title, row.space_title)).collect();
    places.sort();
    assert_eq!(places, [("general".into(), "Moss & Lichen".into()), ("general".into(), "Rustaceans".into())]);
    let tree = chatd.ask(Request::Tree { space: key(Network::Discord, "G1"), offset: 0, limit: 50, peek: true });
    let Response::Tree { groups, conversations, .. } = tree else {
        panic!("not a tree: {tree:?}");
    };
    assert_eq!((groups[0].count, groups[0].kind), (1, GroupKind::Section));
    assert!(conversations.iter().all(|row| row.space_title == "Rustaceans"), "{conversations:?}");

    let kept = chatd.row(&key(Network::Discord, "T-kept"));
    assert_eq!((kept.parent.as_deref(), kept.root.as_deref()), (Some("C1"), Some("100")));
    // Retention runs behind the socket at start.
    for gone in ["T-aged", "T-orphan"] {
        let request = Request::Conversation { conversation: key(Network::Discord, gone) };
        chatd.until(gone, request, |answer| matches!(answer, Response::Error { code: ErrorCode::NotFound, .. }));
        let left = chatd.ask(messages(Network::Discord, gone, 10));
        assert!(matches!(left, Response::Error { code: ErrorCode::NotFound, .. }), "{gone} kept its messages: {left:?}");
    }
    assert!(matches!(chatd.ask(messages(Network::Discord, "T-kept", 10)), Response::Messages { ref messages, .. } if messages.len() == 1));

    // A draft is chatd's to keep, and comes back on the row.
    let conversation = key(Network::Discord, "C1");
    assert_eq!(chatd.ask(Request::SetDraft { conversation: conversation.clone(), text: "half a thou".into() }), Response::Ok);
    assert_eq!(chatd.row(&conversation).draft, "half a thou");
}

#[test]
fn an_account_id_that_is_not_one_plain_file_name_is_never_used() {
    let slack = |team: &str| {
        let session = format!(r#"{{"cookie":"xoxd-test","teams":[{{"token":"xoxc-test","url":"https://example.slack.com/","id":"{team}","name":"Test"}}]}}"#);
        // Whatever starts must not reach a network: these point at a port nothing listens on.
        let env = [
            ("TINYCLIENT_SESSION_SLACK", session.as_str()),
            ("TINYCLIENT_SLACK_API", "http://127.0.0.1:9/"),
            ("TINYCLIENT_SLACK_SOCKET", "ws://127.0.0.1:9"),
        ];
        let chatd = Daemon::with(&[], &env, |_| {});
        // Sessions are read on a thread of their own while the socket already answers.
        let read = chatd.until("the sessions to be read", Request::Status, |answer| matches!(answer, Response::Status { keychain, .. } if keychain.is_settled()));
        let Response::Status { networks, accounts, .. } = read else {
            panic!("not a status");
        };
        let link = networks.iter().find(|status| status.network == Network::Slack).unwrap().link;
        (link, accounts.len(), chatd.dir.join("media").join("slack").exists())
    };
    // An id names a directory under the media cache: these two would name the cache itself
    // and its parent.
    for team in ["..", "."] {
        assert_eq!(slack(team), (Link::SignedOut, 0, false), "{team:?} was taken as an account");
    }
    let (link, accounts, _) = slack("T0.TEST-1");
    assert_eq!((link != Link::SignedOut, accounts), (true, 1), "a plain id must still sign in");
}

#[test]
fn the_sign_in_window_is_told_where_to_open_and_how_it_ended_is_told_apart() {
    let helper = std::env::temp_dir().join(format!("tinyclient-login-{}", std::process::id()));
    std::fs::create_dir_all(&helper).unwrap();
    // A real executable: Windows will not CreateProcess a shell script.
    let source = helper.join("login.rs");
    let script = helper.join(format!("login{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(
        &source,
        r#"fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let dir = std::env::var("TINYCLIENT_LOGIN_DIR").unwrap();
    std::fs::write(format!("{dir}/args"), args).unwrap();
    let code = std::fs::read_to_string(format!("{dir}/code")).unwrap();
    std::process::exit(code.trim().parse().unwrap());
}
"#,
    )
    .unwrap();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let compiled = Command::new(rustc).arg(&source).arg("-o").arg(&script).status().unwrap();
    assert!(compiled.success(), "could not build the sign-in stand-in");
    let chatd = Daemon::with(
        &[],
        &[
            ("TINYCLIENT_LOGIN", &script.to_string_lossy()),
            ("TINYCLIENT_LOGIN_DIR", &helper.to_string_lossy()),
        ],
        |_| {},
    );

    let ended = |code: u8, at| {
        std::fs::write(helper.join("code"), code.to_string()).unwrap();
        match chatd.ask(Request::SignIn { network: Network::Slack, at }) {
            Response::Error { code, detail } => (code, detail, std::fs::read_to_string(helper.join("args")).unwrap()),
            other => panic!("signed in: {other:?}"),
        }
    };
    let (closed, closed_words, args) = ended(1, Some((120, -80)));
    assert_eq!(args.trim(), "slack --at 120,-80");
    let (unreachable, unreachable_words, args) = ended(3, None);
    assert_eq!(args.trim(), "slack", "no position, no flag");
    let (blocked, blocked_words, _) = ended(4, None);
    let (crashed, crashed_words, _) = ended(9, None);
    assert_eq!(
        (closed, unreachable, blocked, crashed),
        (ErrorCode::SignIn, ErrorCode::SignInUnreachable, ErrorCode::SignInBlocked, ErrorCode::SignIn)
    );
    let words = [&closed_words, &unreachable_words, &blocked_words, &crashed_words];
    for (at, one) in words.iter().enumerate() {
        assert!(words.iter().skip(at + 1).all(|other| other != one), "two endings read the same: {one}");
    }
    let _ = std::fs::remove_dir_all(helper);
}

#[test]
fn what_the_user_does_to_a_message_is_shown_and_settled_by_the_network() {
    let chatd = Daemon::demo(&[]);
    let room = demo(SHOWCASE);
    let find = |text: &str| -> Message {
        let messages = chatd.newest(&room);
        messages.into_iter().find(|message| message.text.starts_with(text)).unwrap_or_else(|| panic!("no message {text:?}"))
    };
    let reaction = |message: &Message, emoji: &str| message.reactions.iter().find(|have| have.emoji == emoji).map(|have| (have.count, have.mine));
    let react = |message: &Message, emoji: &str, on| {
        chatd.ask(Request::React { conversation: room.clone(), message_id: message.id.clone(), emoji: emoji.into(), on })
    };

    // A reaction of ours is one reaction, however many times it is told: once by the request
    // and once by the network's own word of it.
    let loved = find("County recorder confirmed");
    assert_eq!(reaction(&loved, "👍"), Some((4, true)));
    assert_eq!(react(&loved, "👍", false), Response::Ok);
    assert_eq!(reaction(&find("County recorder confirmed"), "👍"), Some((3, false)));
    assert_eq!(react(&loved, "👍", true), Response::Ok);
    assert_eq!(react(&loved, "👍", true), Response::Ok, "asking twice is not an error");
    assert_eq!(reaction(&find("County recorder confirmed"), "👍"), Some((4, true)));
    assert_eq!(react(&loved, "🦊", true), Response::Ok);
    assert_eq!(reaction(&find("County recorder confirmed"), "🦊"), Some((1, true)));
    // A custom emoji is named by its file; the network's word for it gives it its name.
    assert_eq!(react(&loved, "emoji-lichen_ok", true), Response::Ok);
    let custom = find("County recorder confirmed");
    let named = custom.reactions.iter().find(|have| have.emoji == ":lichen_ok:").expect("the custom reaction");
    assert_eq!((named.count, named.mine, named.file.as_ref().map(|file| file.id.as_str())), (1, true, Some("emoji-lichen_ok")));
    let nowhere = chatd.ask(Request::React { conversation: room.clone(), message_id: "1".into(), emoji: "👍".into(), on: true });
    assert!(matches!(nowhere, Response::Error { code: ErrorCode::NotFound, .. }), "{nowhere:?}");

    // An edit waits for the network and then reads as edited. Someone else's is refused here.
    let mine = find("Same spot as this one");
    assert!(mine.outgoing && !mine.edited);
    let edit = |message: &Message, text: &str| {
        chatd.ask(Request::Edit { conversation: room.clone(), message_id: message.id.clone(), text: text.into() })
    };
    assert_eq!(edit(&mine, "Same spot, and the camera was level."), Response::Ok);
    let edited = find("Same spot, and the camera was level.");
    assert_eq!((edited.id.as_str(), edited.edited), (mine.id.as_str(), true));
    let theirs = find("I do have the keys");
    let refused = edit(&theirs, "I lost the keys.");
    assert!(matches!(refused, Response::Error { code: ErrorCode::Refused, .. }), "{refused:?}");
    assert_eq!(find("I do have the keys").id, theirs.id);

    // Deleting the message a reply answers leaves the reply saying so.
    let frost = find("Frost on the cushion moss");
    assert_eq!(edited.reply_to.as_ref().map(|quoted| (quoted.id.as_str(), quoted.deleted)), Some((frost.id.as_str(), false)));
    assert_eq!(chatd.ask(Request::Delete { conversation: room.clone(), message_id: frost.id.clone() }), Response::Ok);
    let messages = chatd.newest(&room);
    assert!(messages.iter().all(|message| message.id != frost.id), "the deleted message is still listed");
    let orphaned = find("Same spot, and the camera was level.").reply_to.expect("still a reply");
    assert_eq!((orphaned.deleted, orphaned.snippet.as_str()), (true, ""));
    assert!(orphaned.person.is_some(), "whose it was is still known");

    // Deleting a message a thread hangs off leaves a tombstone, and the thread.
    let root = find("Trail camera build log");
    let thread = root.thread.clone().expect("a thread");
    assert_eq!((thread.participants.len(), thread.unread), (3, 2));
    assert_eq!(chatd.ask(Request::Delete { conversation: room.clone(), message_id: root.id.clone() }), Response::Ok);
    let tomb = chatd.newest(&room).into_iter().find(|message| message.id == root.id).expect("the tombstone");
    assert_eq!((tomb.deleted, tomb.text.as_str(), tomb.thread.as_ref()), (true, "", Some(&thread)));
    let hanging = chatd.row(&demo(&thread.id));
    assert_eq!((hanging.parent.as_deref(), hanging.root.as_deref()), (Some(SHOWCASE), Some(root.id.as_str())));
    assert_eq!(chatd.newest(&demo(&thread.id)).len(), 6);

    // A message sent is accepted by the network and takes the network's id.
    let sent = chatd.ask(Request::Send { conversation: room.clone(), client_id: "9001".into(), text: "Kettle is on.".into(), reply_to: None, with: Default::default() });
    assert!(matches!(sent, Response::Sent { ref message } if message.state == SendState::Pending), "{sent:?}");
    let request = Request::Messages { conversation: room.clone(), page: Page::Newest, limit: 200 };
    chatd.until("the send to be accepted", request, |answer| {
        let Response::Messages { messages, .. } = answer else { return false };
        messages.iter().any(|message| message.text == "Kettle is on." && message.state == SendState::Sent && message.id != "9001")
    });
}

#[test]
fn people_are_answered_looked_up_and_searched_for_only_when_the_typing_pauses() {
    let chatd = Daemon::demo(&[]);
    let account = demo("").account();

    // A friend request is answered by the user's own request and by nothing else.
    let requests = |relation| {
        let request = Request::People { network: Some(Network::Discord), relation: Some(relation), query: None, offset: 0, limit: 50 };
        let Response::People { people, .. } = chatd.ask(request) else { panic!("not people") };
        people
    };
    let dorian = requests(Relation::Incoming).pop().expect("a request waiting");
    assert_eq!(dorian.link, format!("discord://-/users/{}", dorian.key.id), "the network's own link to a person");
    assert_eq!(chatd.ask(Request::Relate { person: dorian.key.clone(), action: RelateAction::Accept }), Response::Ok);
    assert!(requests(Relation::Incoming).is_empty());
    assert!(requests(Relation::Friend).iter().any(|friend| friend.key == dorian.key));
    let sent = requests(Relation::Outgoing).pop().expect("a request we sent");
    assert_eq!(chatd.ask(Request::Relate { person: sent.key, action: RelateAction::Cancel }), Response::Ok);
    assert!(requests(Relation::Outgoing).is_empty());

    // Our own presence and status, on one account.
    let own = |chatd: &Daemon| {
        let Response::Status { accounts, .. } = chatd.ask(Request::Status) else { panic!("not a status") };
        let ours = accounts.into_iter().find(|status| status.key == account).unwrap();
        (ours.presence, ours.status, ours.avatar.is_some())
    };
    assert_eq!(own(&chatd), (Presence::Online, String::new(), true));
    let set = Request::SetPresence { account: account.clone(), presence: Presence::Dnd, status: Some("heads down".into()) };
    assert_eq!(chatd.ask(set), Response::Ok);
    assert_eq!(own(&chatd), (Presence::Dnd, "heads down".to_owned(), true));
    // No text given leaves the text as it is.
    assert_eq!(chatd.ask(Request::SetPresence { account: account.clone(), presence: Presence::Idle, status: None }), Response::Ok);
    assert_eq!(own(&chatd), (Presence::Idle, "heads down".to_owned(), true));

    let ines = demo("220000000000000001");
    let Response::Profile { profile } = chatd.ask(Request::Profile { person: ines.clone(), space: None }) else {
        panic!("not a profile");
    };
    assert_eq!((profile.person.key, profile.person.name.as_str()), (ines, "Ines Marchetti"));
    // Discord says when we became friends and has no time zone for anyone.
    assert!(profile.friends_since.is_some() && profile.time_zone.is_empty() && profile.utc_offset.is_none());
    assert!((1..=protocol::MUTUAL_SPACES).contains(&profile.mutual_spaces.len()), "{:?}", profile.mutual_spaces);
    let stranger = chatd.ask(Request::Profile { person: demo("1"), space: None });
    assert!(matches!(stranger, Response::Error { code: ErrorCode::NotFound, .. }), "{stranger:?}");

    // Searching: each keystroke is a request, and the network hears one query once the typing
    // has paused. The demo's network says `Searched` for every query it is given.
    let search = |query: &str| {
        let request = Request::People { network: Some(Network::Discord), relation: None, query: Some(query.into()), offset: 0, limit: 20 };
        assert!(matches!(chatd.ask(request), Response::People { .. }));
    };
    let searched = |changes: Vec<Change>| -> Vec<String> {
        let queries = changes.into_iter().filter_map(|change| match change {
            Change::Searched { query, .. } => Some(query),
            _ => None,
        });
        queries.collect()
    };
    let seq = chatd.seq();
    search("y");
    assert_eq!(searched(chatd.changes(seq, Duration::from_millis(900))), Vec::<String>::new(), "one letter reached the network");

    let seq = chatd.seq();
    for query in ["yu", "yus", "yusu", "yusuf"] {
        search(query);
        sleep(Duration::from_millis(60));
    }
    let heard = searched(chatd.changes(seq, Duration::from_millis(1_500)));
    assert_eq!(heard, ["yusuf"], "only the query the typing stopped on");
    // Reading the list again with the same query, as a shell does after every result, is not
    // a new search.
    let seq = chatd.seq();
    search("yusuf");
    assert_eq!(searched(chatd.changes(seq, Duration::from_millis(900))), Vec::<String>::new());
}

#[test]
fn a_voice_channel_says_who_is_in_it_and_a_call_that_fails_takes_us_out_of_it() {
    let chatd = Daemon::demo(&[]);
    let campfire = demo(CAMPFIRE);
    let names = |row: &Conversation| -> Vec<String> {
        row.occupants.iter().flat_map(|occupants| &occupants.people).map(|who| who.name.clone()).collect()
    };

    // The tree's rows carry who is in each voice channel, bounded, with the true total.
    let tree = chatd.ask(Request::Tree { space: demo(MOSS), offset: 0, limit: 500, peek: true });
    let Response::Tree { conversations, .. } = tree else { panic!("not a tree: {tree:?}") };
    let voice = |title: &str| conversations.iter().find(|row| row.title == title).unwrap();
    assert_eq!(names(voice("Campfire")), ["Ines Marchetti", "Tomasz Brandt", "Wren Halloran"]);
    let seats = &voice("Campfire").occupants.as_ref().unwrap().people;
    assert_eq!((seats[1].muted, seats[2].streaming, seats[0].muted), (true, true, false));
    let full = voice("Quiet room").occupants.as_ref().unwrap();
    assert_eq!((full.people.len(), full.total), (OCCUPANTS_LIMIT, 31));
    assert!(voice("general").occupants.is_none());

    // Joining puts us in it on the network. The shell hears of that row alone: the tree is
    // not announced, so it is not read again.
    let seq = chatd.seq();
    let started = chatd.ask(Request::CallStart { conversation: campfire.clone() });
    assert!(matches!(started, Response::Status { media: Media::Running, .. }), "{started:?}");
    let told = chatd.changes(seq, Duration::from_millis(600));
    assert!(told.iter().any(|change| matches!(change,
        Change::Occupants { conversation, occupants } if *conversation == campfire && occupants.total == 4)), "{told:?}");
    assert!(!told.iter().any(|change| matches!(change, Change::Tree { .. })), "{told:?}");
    assert_eq!(names(&chatd.row(&campfire)).last().map(String::as_str), Some("Robin Vale"));

    // mediad dies. The call is a failure in words, and the network no longer shows us there.
    let pid = chatd.mediad_pids()[0];
    assert!(Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    chatd.until("the failure", Request::Status, |status| failed_with(status, CallFailure::Engine));
    let request = Request::Conversation { conversation: campfire.clone() };
    let left = |answer: &Response| matches!(answer, Response::Conversation { conversation } if names(conversation).len() == 3);
    chatd.until("leaving the channel after mediad died", request.clone(), left);

    // Dismissed, then tried again; this time nothing ever answers. The deadline is the
    // failure, and again we are taken out of the channel.
    chatd.ask(Request::CallEnd);
    chatd.ask(Request::CallStart { conversation: campfire.clone() });
    assert_eq!(names(&chatd.row(&campfire)).len(), 4);
    let deadline = Instant::now() + Duration::from_secs(13);
    while !failed_with(&chatd.ask(Request::Status), CallFailure::Lost) {
        assert!(Instant::now() < deadline, "the call never timed out");
        sleep(Duration::from_millis(100));
    }
    chatd.until("leaving the channel after the deadline", request, left);
    assert_eq!(chatd.mediad_pids(), Vec::<u32>::new());
}

#[test]
fn a_picture_is_fetched_again_larger_when_it_is_drawn_larger_and_never_smaller() {
    let chatd = Daemon::demo(&[]);
    let account = demo("").account();
    let emoji = "emoji-mossparrot".to_owned();
    let fetch = |size| {
        let request = Request::Fetch { account: account.clone(), ids: vec![emoji.clone()], size, retry: false };
        let Response::Media { mut files } = chatd.ask(request) else { panic!("not media") };
        files.pop().expect("the emoji is registered")
    };
    let landed = |since: u64| -> u32 {
        let changes = chatd.changes(since, Duration::from_secs(4));
        let path = changes.into_iter().find_map(|change| match change {
            Change::Media { file, .. } if file.id == emoji => file.path,
            _ => None,
        });
        png_width(&path.expect("the larger picture landed"))
    };

    // As registered: 48 pixels, already on disk.
    let small = fetch(None);
    assert_eq!((small.width, png_width(small.path.as_deref().unwrap())), (48, 48));
    assert!(fetch(Some(40)).path.is_some(), "what is held is large enough");

    // Drawn at 40 points on a 2x screen: the 96 pixel size the network also serves.
    let seq = chatd.seq();
    let asked = fetch(Some(80));
    assert_eq!((asked.path, asked.width), (None, 48), "the row keeps its layout size while the larger one is fetched");
    assert_eq!(landed(seq), 96);
    // Asked small again, it stays large: nothing is fetched twice to save pixels.
    assert_eq!(png_width(fetch(Some(48)).path.as_deref().unwrap()), 96);
    // Larger than anything registered: the largest there is.
    let seq = chatd.seq();
    assert_eq!(fetch(Some(2_000)).path, None);
    assert_eq!(landed(seq), 144);
    assert_eq!(png_width(fetch(Some(4_000)).path.as_deref().unwrap()), 144, "nothing larger to ask for");
}

#[test]
fn signing_out_waits_for_the_network_to_end_the_session_but_not_for_long() {
    let signed_out = |network_takes: &str| {
        let chatd = Daemon::demo(&[("TINYCLIENT_DEMO_SIGN_OUT_MS", network_takes)]);
        let account = harbor("").account();
        let began = Instant::now();
        let Response::Status { accounts, .. } = chatd.ask(Request::SignOut { account: account.clone() }) else {
            panic!("not a status");
        };
        assert!(accounts.iter().all(|status| status.key != account), "still signed in");
        // And everything stored for it is gone.
        let gone = chatd.ask(Request::Tree { space: harbor("T04HARBOR1"), offset: 0, limit: 10, peek: true });
        assert!(matches!(gone, Response::Error { code: ErrorCode::NotFound, .. }), "{gone:?}");
        began.elapsed()
    };
    let waited = signed_out("900");
    assert!(waited >= Duration::from_millis(900) && waited < Duration::from_secs(4), "{waited:?}");
    // A network that never answers holds sign-out for five seconds and no more.
    let waited = signed_out("60000");
    assert!(waited >= Duration::from_secs(5) && waited < Duration::from_secs(8), "{waited:?}");
}

#[test]
fn a_workspace_keeps_its_direct_conversations_in_its_own_tree() {
    let chatd = Daemon::demo(&[]);
    let tree = chatd.ask(Request::Tree { space: harbor("T04HARBOR1"), offset: 0, limit: 500, peek: true });
    let Response::Tree { groups, conversations, .. } = tree else { panic!("not a tree: {tree:?}") };
    let directs = groups.iter().find(|group| group.kind == GroupKind::Directs).expect("a group of direct conversations");
    let under: Vec<&Conversation> = conversations.iter().filter(|row| row.group.as_deref() == Some(directs.id.as_str())).collect();
    assert_eq!(under.len() as u32, directs.count);
    assert!(under.iter().all(|row| row.kind.is_direct() && row.space_title == "Harbor Labs"), "{under:?}");
    assert!(under.iter().any(|row| row.kind == ConversationKind::Group));
    // They are still in the Inbox, where every network's direct conversations are.
    let request = Request::Inbox { network: Some(Network::Slack), filter: InboxFilter::Directs, offset: 0, limit: 50 };
    let Response::Conversations { conversations, .. } = chatd.ask(request) else { panic!("not a list") };
    for row in under {
        assert!(conversations.iter().any(|listed| listed.key == row.key), "{} is not in the Inbox", row.title);
    }
    // A Slack call is the official app's to carry: asking for it here is refused, and the
    // hand-off link is what the button gets.
    let general = harbor("C04CHANNEL00");
    let refused = chatd.ask(Request::CallStart { conversation: general.clone() });
    assert!(matches!(refused, Response::Error { code: ErrorCode::Unsupported, .. }), "{refused:?}");
    assert!(matches!(chatd.ask(Request::Handoff { conversation: general }), Response::Link { ref url } if url.contains("huddle")));
}

#[test]
fn what_was_shared_in_a_conversation_is_listed_by_kind_newest_first() {
    let chatd = Daemon::demo(&[]);
    let shared = |kind, before: Option<String>, limit| {
        let request = Request::Shared { conversation: demo(SHOWCASE), kind, before, limit, search: false };
        match chatd.ask(request) {
            Response::Shared { items, more, .. } => (items, more),
            other => panic!("not shared: {other:?}"),
        }
    };
    let (media, more) = shared(protocol::SharedKind::Media, None, 5);
    assert!(more, "the showcase holds more than five pictures");
    assert_eq!(media.len(), 5);
    assert!(media.windows(2).all(|pair| pair[0].sent >= pair[1].sent), "newest first");
    let kinds: Vec<MediaKind> = media.iter().map(|item| item.attachment.as_ref().unwrap().file.kind).collect();
    assert!(kinds.iter().all(|kind| matches!(kind, MediaKind::Image | MediaKind::Video | MediaKind::Gif)), "{kinds:?}");
    assert!(media.iter().all(|item| !item.attachment.as_ref().unwrap().sticker), "a sticker is not shared media");
    // A thumbnail already on disk comes with its path, so the pane draws without asking.
    assert!(media.iter().any(|item| item.attachment.as_ref().unwrap().thumb.as_ref().is_some_and(|thumb| thumb.path.is_some())));
    // The page below continues under the last message listed.
    let (older, _) = shared(protocol::SharedKind::Media, media.last().map(|item| item.message_id.clone()), 5);
    assert!(older.iter().all(|item| item.sent <= media[4].sent && item.message_id != media[4].message_id));

    let (files, _) = shared(protocol::SharedKind::Files, None, 50);
    let mut names: Vec<&str> = files.iter().map(|item| item.attachment.as_ref().unwrap().name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Trail camera wiring.pdf", "camera-logs.zip", "voice-message.wav"]);
    let (links, _) = shared(protocol::SharedKind::Links, None, 50);
    let lepraria = links.iter().find(|item| item.url == "https://example.org/wiki/Lepraria").expect("the previewed link");
    assert_eq!((lepraria.title.as_str(), lepraria.sender.as_str()), ("Lepraria", "Wren Halloran"));
    assert_eq!(links.iter().filter(|item| item.url == lepraria.url).count(), 1, "a link and its preview are one link");
    assert!(links.iter().any(|item| item.url == "https://example.org/foray/sign-up" && item.title.is_empty()));

    let nowhere = chatd.ask(Request::Shared { conversation: demo("1"), kind: protocol::SharedKind::Media, before: None, limit: 5, search: false });
    assert!(matches!(nowhere, Response::Error { code: ErrorCode::NotFound, .. }), "{nowhere:?}");
    // A profile says when they joined and what the network calls its own details.
    let Response::Profile { profile } = chatd.ask(Request::Profile { person: demo("220000000000000001"), space: None }) else {
        panic!("not a profile");
    };
    assert!(profile.since.is_some() && profile.accent.is_some());
    // Every detail is in its typed place: nothing is left as a labelled line.
    assert!(profile.fields.is_empty(), "{:?}", profile.fields);
}
