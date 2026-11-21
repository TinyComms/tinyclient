//! Issue #794: "Search everywhere" fans out to connected networks that support global search,
//! merges hits by time, supports filters & pagination, and reports networks not searched.

use protocol::{
    read_frame, write_frame, ChatStream, Network, Request, Response, SearchFilters, SearchHas,
};
use std::collections::HashSet;
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
            "/tmp/tinyclient-search-{}-{}",
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
fn everywhere_search_merges_two_networks_and_reports_unsearched() {
    let daemon = Daemon::demo();

    // Common word "the" appears across demo Slack and Telegram accounts.
    let Response::MessageHits { hits, cursor, not_searched } = daemon.ask(&Request::SearchNetwork {
        conversation: None,
        space: None,
        account: None,
        network: None,
        query: "the".into(),
        filters: SearchFilters::default(),
        cursor: None,
    }) else {
        panic!("expected MessageHits");
    };
    assert!(!hits.is_empty(), "everywhere search should return hits");

    // Verify hits are merged newest-first.
    for pair in hits.windows(2) {
        assert!(pair[0].sent >= pair[1].sent, "hits must be ordered newest first");
    }

    // Verify hits come from at least two networks (Slack and Telegram) when we also fetch all pages.
    let mut networks: HashSet<Network> = hits.iter().map(|h| h.conversation.network).collect();
    let mut next = cursor.clone();
    while let Some(cur) = next {
        let Response::MessageHits { hits: more, cursor: next_cur, .. } = daemon.ask(&Request::SearchNetwork {
            conversation: None,
            space: None,
            account: None,
            network: None,
            query: "the".into(),
            filters: SearchFilters::default(),
            cursor: Some(cur),
        }) else {
            panic!("expected MessageHits on next page");
        };
        assert!(!more.is_empty());
        networks.extend(more.iter().map(|h| h.conversation.network));
        next = next_cur;
    }
    assert!(
        networks.contains(&Network::Slack) && networks.contains(&Network::Telegram),
        "expected merged hits from Slack and Telegram, got {networks:?}"
    );

    // Honest footer: Signal, Teams, and Discord (no global search) are named in not_searched.
    assert!(
        not_searched.contains(&Network::Signal),
        "Signal should be in not_searched: {not_searched:?}"
    );
    assert!(
        not_searched.contains(&Network::Teams),
        "Teams should be in not_searched: {not_searched:?}"
    );
    assert!(
        !not_searched.contains(&Network::Slack) && !not_searched.contains(&Network::Telegram),
        "searched networks must not appear in not_searched: {not_searched:?}"
    );

    // Network-scoped search narrows to one network.
    let Response::MessageHits { hits: slack_only, .. } = daemon.ask(&Request::SearchNetwork {
        conversation: None,
        space: None,
        account: None,
        network: Some(Network::Slack),
        query: "the".into(),
        filters: SearchFilters::default(),
        cursor: None,
    }) else {
        panic!("expected MessageHits");
    };
    assert!(!slack_only.is_empty());
    assert!(slack_only.iter().all(|h| h.conversation.network == Network::Slack));

    // Filter test: mentions_me / has: Links accepted cleanly without error.
    let filtered = daemon.ask(&Request::SearchNetwork {
        conversation: None,
        space: None,
        account: None,
        network: None,
        query: String::new(),
        filters: SearchFilters {
            has: Some(SearchHas::Links),
            ..SearchFilters::default()
        },
        cursor: None,
    });
    assert!(matches!(filtered, Response::MessageHits { .. }), "got {filtered:?}");
}
