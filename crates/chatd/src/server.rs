use crate::adapters::{self, About, Adapter, Command, Picture, Refusal, Reply, SearchScope};
use crate::blobs::Blobs;
use crate::hub::{lock, now, Hub, KEYCHAIN_PROMPT};
use crate::media::CallError;
use crate::session;
use crate::store::Store;
use protocol::settings;
use protocol::{
    clip, read_frame_bytes, support_dir, write_frame, CatalogKind, ConversationInfo, MediaFile, MediaKind, Outgoing, SharedKind, SpaceInfo, AccountKey, AccountStatus, CallSupport, Capabilities,
    Change, ChatListener, ChatStream, Conversation, ConversationKind, ErrorCode, InboxCounts, Key,
    Link, Media, MediaRequest, Message, MessageKind, Network, NetworkStatus, Page, Person, Presence,
    Profile, Reaction, RelateAction, Relation, Request, Response, SendState, Sender, Watching,
    MUTUAL_SPACES, PAGE_LIMIT, SEARCH_MIN_CHARS, WATCH_LIMIT,
};
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

/// How long `wait` holds a request open when nothing changes.
const WAIT: Duration = Duration::from_secs(30);
/// How long an adapter has to answer a command the shell is waiting on.
const ANSWER: Duration = Duration::from_secs(30);
/// A message still pending after this many seconds did not go.
const SEND_SECONDS: i64 = 60;
/// How many text sends one account may hold while it is offline.
const QUEUE_LIMIT: usize = 32;
/// A queued send older than this is not sent when the link returns.
const QUEUE_AGE: i64 = 24 * 60 * 60;
/// Shown on a queued send that aged out, with Retry under it.
const QUEUE_LATE: &str = "This waited more than a day. Send it if you still want it to go.";
/// Most conversations of a Space's tree in one answer; they are small rows.
const TREE_LIMIT: u32 = 500;
/// A people search reaches a network once the typing has paused this long, so at most one
/// query per account in that time and never one per keystroke.
const SEARCH_PAUSE: Duration = Duration::from_millis(400);
/// The same query is not sent to an account again within this long: the shell reads `People`
/// again after every result, with the query still in the box.
const SEARCH_AGAIN: Duration = Duration::from_secs(60);
/// How long sign-out waits for the adapter to end the session on the network.
const SIGN_OUT: Duration = Duration::from_secs(5);
/// The largest pixel size a `Fetch` may ask a picture in.
const LARGEST: u32 = 4_096;

type Adapters = Arc<Mutex<HashMap<AccountKey, Adapter>>>;

/// People searches on their way to the networks: per account, the newest query and when it is
/// due, and the last one sent.
#[derive(Default)]
struct Searches {
    state: Mutex<Searching>,
    wake: Condvar,
}

#[derive(Default)]
struct Searching {
    waiting: HashMap<AccountKey, (String, Instant)>,
    sent: HashMap<AccountKey, (String, Instant)>,
}

impl Searches {
    /// Sends each query when its pause is over. With nothing waiting this sleeps on the condvar
    /// with no timeout, so an idle chatd is not woken for it.
    fn run(&self, adapters: &Adapters) {
        let mut state = lock(&self.state);
        loop {
            let at = Instant::now();
            let due: Vec<AccountKey> = state.waiting.iter().filter(|(_, (_, due))| *due <= at).map(|(account, _)| account.clone()).collect();
            for account in due {
                let Some((query, _)) = state.waiting.remove(&account) else {
                    continue;
                };
                state.sent.retain(|_, (_, sent)| sent.elapsed() < SEARCH_AGAIN);
                state.sent.insert(account.clone(), (query.clone(), at));
                if let Some(adapter) = lock(adapters).get(&account) {
                    adapter.command(Command::SearchPeople(query));
                }
            }
            state = match state.waiting.values().map(|(_, due)| *due).min() {
                Some(next) => self.wake.wait_timeout(state, next.saturating_duration_since(at)).unwrap_or_else(PoisonError::into_inner).0,
                None => self.wake.wait(state).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// True when this query was already sent. The caller names that account on the `People`
    /// answer. It is not a new search: the shell reads the list again after every result, and
    /// nothing is said on the hub. A query already waiting is left to finish. Anything else is
    /// queued after the pause.
    fn ask(&self, account: AccountKey, query: &str) -> bool {
        let mut state = lock(&self.state);
        let fresh = state.sent.get(&account).filter(|(_, at)| at.elapsed() < SEARCH_AGAIN).map(|(sent, _)| sent.clone());
        let waiting = state.waiting.get(&account).map(|(waiting, _)| waiting.clone());
        match search_fate(fresh.as_deref(), waiting.as_deref(), query) {
            SearchFate::Repeated => {
                state.waiting.remove(&account);
                true
            }
            SearchFate::Waiting => false,
            SearchFate::Queue => {
                state.waiting.insert(account, (query.to_owned(), Instant::now() + SEARCH_PAUSE));
                drop(state);
                self.wake.notify_all();
                false
            }
        }
    }
}

/// What to do with a people search the shell just asked for again.
enum SearchFate {
    /// Sent recently. The shell is reading again, which is not a new search.
    Repeated,
    /// The same letters are already waiting out the pause.
    Waiting,
    /// New letters: send them once the pause is over.
    Queue,
}

fn search_fate(fresh_sent: Option<&str>, waiting: Option<&str>, query: &str) -> SearchFate {
    if fresh_sent == Some(query) {
        SearchFate::Repeated
    } else if waiting == Some(query) {
        SearchFate::Waiting
    } else {
        SearchFate::Queue
    }
}

pub struct Chat {
    pub(crate) hub: Arc<Hub>,
    adapters: Adapters,
    /// History catch-ups asked before that account's adapter was in the map.
    held: Mutex<Vec<(AccountKey, Command)>>,
    blobs: Blobs,
    searches: Arc<Searches>,
    /// Itself, for the work a request starts that outlives the request (`ReadSessions`).
    this: OnceLock<Weak<Chat>>,
    /// Running inside an app (`start_in_process`): a `Shutdown` must not end the process.
    in_process: bool,
}

/// A refusal on its way out of a helper. Boxed, because a `Response` is large.
type Refused = Box<Response>;

/// Text with no file, voice note, picker item, or scheduled time. Those still need a connection.
fn queueable(with: &Outgoing) -> bool {
    with.files.is_empty() && with.voice.is_none() && with.pick.is_none() && with.schedule.is_none()
}

/// What a refused offline send says. A file names that it needs a connection. Nothing here
/// promises the send will go by itself.
fn offline_words(account: &AccountKey, with: &Outgoing) -> String {
    let name = account.network.label();
    if with.files.is_empty() && with.voice.is_none() {
        format!("{name} is offline.")
    } else {
        format!("{name} is offline. Files need a connection.")
    }
}

fn error(code: ErrorCode, detail: impl ToString) -> Response {
    Response::Error {
        code,
        detail: detail.to_string(),
    }
}

fn internal(err: rusqlite::Error) -> Response {
    error(ErrorCode::Internal, err)
}

fn call_error(err: CallError) -> Response {
    match err {
        CallError::Active => error(ErrorCode::CallActive, "already in a call"),
        CallError::NoCall => error(ErrorCode::NoCall, "not in a call"),
        CallError::Media(err) => error(ErrorCode::Media, err),
    }
}

fn response_detail(response: &Response) -> String {
    match response {
        Response::Error { detail, .. } => detail.clone(),
        _ => "Couldn't place that call. Try again.".to_owned(),
    }
}

/// `TINYCLIENT_LOGIN`, or the `tinyclient-login` next to this binary.
pub(crate) fn login_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("TINYCLIENT_LOGIN") {
        return Ok(PathBuf::from(path));
    }
    Ok(std::env::current_exe()?.with_file_name(format!("tinyclient-login{}", std::env::consts::EXE_SUFFIX)))
}

/// How a web sign-in that did not finish ended, in the user's words: the helper's exit code
/// (`crates/login`), or the same code from `login::web` on a phone.
fn sign_in_ended(network: Network, code: Option<i32>) -> Response {
    let name = network.label();
    match code {
        Some(1) => error(ErrorCode::SignIn, "The sign-in window was closed."),
        Some(3) => error(ErrorCode::SignInUnreachable, format!("{name}'s sign-in page could not be loaded. Check the connection and try again.")),
        Some(4) => error(ErrorCode::SignInBlocked, format!("Your organization does not allow signing in to {name} this way.")),
        _ => error(ErrorCode::SignIn, "The sign-in window stopped before it finished."),
    }
}

/// `TINYCLIENT_TEST_KEYCHAIN_MS`: in a scratch directory, a session read takes this long, as one
/// held up by a keychain prompt does. Never read otherwise.
fn test_keychain_delay() -> Option<Duration> {
    if session::enabled() {
        return None;
    }
    std::env::var("TINYCLIENT_TEST_KEYCHAIN_MS").ok()?.parse().ok().map(Duration::from_millis)
}

/// `TINYCLIENT_TEST_PANIC`: in a scratch directory, a `Find` for this query panics while it holds
/// the store, as a bug in one request would. Never read otherwise.
fn test_panic(query: &str) -> bool {
    !session::enabled() && std::env::var("TINYCLIENT_TEST_PANIC").is_ok_and(|panics| panics == query)
}

impl Chat {
    /// Everything but the sessions: those are read by [`serve`] once the socket answers
    /// (`read_sessions`), because a keychain read can wait on the user for as long as they like.
    pub fn new(store: Store) -> Chat {
        Chat::build(store, false)
    }

    /// The same core for a process that is an app (phones): see `shutdown`.
    pub fn in_process(store: Store) -> Chat {
        Chat::build(store, true)
    }

    fn build(store: Store, in_process: bool) -> Chat {
        let hub = Arc::new(Hub::new(store));
        let chat = Chat {
            blobs: Blobs::new(hub.clone(), support_dir().join("media")),
            hub,
            adapters: Arc::default(),
            held: Mutex::new(Vec::new()),
            searches: Arc::default(),
            this: OnceLock::new(),
            in_process,
        };
        // A call that fails here (mediad died, a deadline passed) is still up on the network
        // until the adapter leaves: the other side would go on ringing, the channel go on
        // showing us.
        let adapters = chat.adapters.clone();
        chat.hub.calls.on_failure(move |conversation| {
            if let Some(adapter) = lock(&adapters).get(&conversation.account()) {
                adapter.command(Command::CallLeave(conversation.id.clone()));
            }
        });
        // What mediad has for a Tiny server's live socket (its end-to-end encryption) goes out
        // through that account's adapter.
        let adapters = chat.adapters.clone();
        chat.hub.calls.on_signal(move |conversation, frame| {
            if let Some(adapter) = lock(&adapters).get(&conversation.account()) {
                adapter.command(Command::Tiny(adapters::tiny::live::Ask::E2ee { frame }));
            }
        });
        let adapters = chat.adapters.clone();
        chat.hub.calls.on_offer(move |conversation, sdp| {
            if let Some(adapter) = lock(&adapters).get(&conversation.account()) {
                adapter.command(Command::Tiny(adapters::tiny::live::Ask::Offer { sdp }));
            }
        });
        // Tiny media dropped, or came back, while the call is still up. The live socket joins
        // again, or notes that the link held. A Discord call does this inside mediad.
        let adapters = chat.adapters.clone();
        chat.hub.calls.on_tiny_down(move |conversation| {
            if conversation.network != Network::Tiny {
                return;
            }
            if let Some(adapter) = lock(&adapters).get(&conversation.account()) {
                adapter.command(Command::Tiny(adapters::tiny::live::Ask::Resume));
            }
        });
        let adapters = chat.adapters.clone();
        chat.hub.calls.on_tiny_up(move |conversation| {
            if conversation.network != Network::Tiny {
                return;
            }
            if let Some(adapter) = lock(&adapters).get(&conversation.account()) {
                adapter.command(Command::Tiny(adapters::tiny::live::Ask::Settled));
            }
        });
        let (searches, adapters) = (chat.searches.clone(), chat.adapters.clone());
        let spawned = std::thread::Builder::new().name("search".into()).stack_size(256 * 1024).spawn(move || searches.run(&adapters));
        if let Err(err) = spawned {
            tracing::warn!("chatd: people search: {err}");
        }
        // A seeded demo serves its own accounts.
        lock(&chat.adapters).extend(crate::demo::attach(&chat.hub));
        // Whatever was on its way when chatd last stopped did not go. A text send queued while
        // the account was offline stays, unless it has already waited more than a day.
        let _ = chat.hub.store().fail_pending(i64::MAX, "tinyclient was restarted before this was sent.");
        let _ = chat.hub.store().fail_queued(now() - QUEUE_AGE, QUEUE_LATE);
        // Expiry and retention run behind the socket. A message whose own timer ran out goes at
        // its moment, online or not — the thread sleeps until then, at most a minute, and
        // wakes sooner when a message with a timer arrived. Retention runs about a minute after
        // the boot, then once a day, and when its setting changes. Never inside the boot: the
        // sign-in writes months of chats into the store while this would be deleting rows of
        // it (a minute's expiry is one indexed delete, and fine even then). A thread sleeping
        // between the runs costs nothing.
        const AFTER_BOOT: Duration = Duration::from_secs(60);
        const EXPIRY_EVERY: Duration = Duration::from_secs(60);
        const RETENTION_EVERY: Duration = Duration::from_secs(86_400);
        let hub = chat.hub.clone();
        std::thread::spawn(move || {
            let boot = Instant::now();
            // Retention ran last then; its first run comes at the boot's minute.
            let mut pruned: Option<Instant> = None;
            loop {
                // Sleep until the next message's own timer runs out, at most a minute — its
                // time comes whether the account is online or not — and shorter still when a
                // message with a timer arrived or retention is asked for. Until the boot's
                // minute the sleep is capped there too: the sign-in has the store busy.
                let cap = if pruned.is_none() { AFTER_BOOT } else { EXPIRY_EVERY };
                let wait = match hub.store().until_expiry(now()) {
                    Ok(at) => at.map_or(cap, |at| at.min(cap)),
                    Err(_) => cap,
                };
                let (asked, _) = hub.prune_due(wait);
                // Whatever ran out is gone from the store, online or not; the conversations it
                // was in are read again.
                match hub.store().expire(now()) {
                    Ok(gone) => {
                        for conversation in gone {
                            hub.changed(Change::Messages { conversation: conversation.clone() });
                            hub.changed(Change::Conversation { conversation });
                        }
                    }
                    Err(err) => tracing::warn!("chatd: messages past their time: {err}"),
                }
                // Retention: about a minute after the boot, then once a day, or at once when
                // asked. Its scans are worth skipping while the boot's sign-in writes months
                // of chats; a minute's expiry is one indexed delete and waits its turn.
                if pruned.is_some_and(|at| at.elapsed() >= RETENTION_EVERY)
                    || asked
                    || (pruned.is_none() && boot.elapsed() >= AFTER_BOOT)
                {
                    let days = hub.settings().number(settings::KEEP_DAYS);
                    match hub.store().prune(now(), days) {
                        Ok(0) | Err(_) => {}
                        Ok(gone) => {
                            tracing::warn!("chatd: pruned {gone} old messages");
                            // What is on screen may be gone.
                            hub.reset();
                        }
                    }
                    pruned = Some(Instant::now());
                }
            }
        });
        chat
    }

    /// Reads each network's session from the keychain and connects its accounts, on a thread of
    /// its own, while the socket answers. `Status.keychain` says what it is doing; a read that
    /// sits behind a macOS prompt is `waiting`. After the first read of every network, the
    /// cache of accounts with no session left goes.
    pub fn read_sessions(chat: &Arc<Chat>, networks: Vec<Network>, first: bool) {
        let networks: Vec<Network> = networks.into_iter().filter(|network| adapters::supported(*network)).collect();
        chat.hub.reading(&networks);
        let chat = chat.clone();
        let spawned = std::thread::Builder::new().name("sessions".into()).stack_size(256 * 1024).spawn(move || {
            // The networks whose accounts are known for certain: no session at all, or a session
            // whose every account started. Only their leftovers are forgotten. A start that
            // failed, a refused keychain or a network this build does not speak keeps its cache.
            let mut settled = Vec::new();
            for network in networks {
                match chat.read_session(network) {
                    session::Stored::Found(session) => {
                        match chat.connect(network, &session) {
                            Ok(()) => settled.push(network),
                            Err(err) => {
                                tracing::warn!("chatd: {}: {err}; keeping its cache", network.label());
                                // accounts() or start can fail before an adapter is inserted.
                                // Without a link, status drops the account as SignedOut, shows()
                                // is false, and the open pane clears even though the cache stays.
                                chat.keep_cached_offline(network, &err);
                            }
                        }
                        chat.hub.read_now(network, false, false);
                    }
                    session::Stored::Absent => {
                        settled.push(network);
                        chat.hub.read_now(network, false, false);
                    }
                    // A refused keychain prompt is not a sign-out. What was cached stays, and
                    // `ReadSessions` (or the next start) asks again.
                    session::Stored::Unreadable(why) => {
                        tracing::warn!("chatd: {}: the keychain did not hand over the session ({why}); keeping its cache", network.label());
                        chat.hub.read_now(network, false, true);
                    }
                }
            }
            if first {
                chat.forget_strangers(&settled);
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("chatd: sessions: {err}");
        }
    }

    /// One network's session. The read itself runs on a thread of its own so that one not
    /// answered within [`KEYCHAIN_PROMPT`] can be told as a prompt on screen while it waits.
    fn read_session(&self, network: Network) -> session::Stored {
        self.hub.read_now(network, true, false);
        let (done, read) = channel();
        let spawned = std::thread::Builder::new().name("keychain".into()).stack_size(128 * 1024).spawn(move || {
            if let Some(delay) = test_keychain_delay() {
                std::thread::sleep(delay);
            }
            let _ = done.send(session::read_waiting(network));
        });
        if let Err(err) = spawned {
            return session::Stored::Unreadable(err.to_string());
        }
        match read.recv_timeout(KEYCHAIN_PROMPT) {
            Ok(stored) => stored,
            Err(_) => {
                self.hub.read_waiting();
                read.recv().unwrap_or_else(|_| session::Stored::Unreadable("the keychain read ended without an answer".to_owned()))
            }
        }
    }

    /// Marks every stored account of `network` Offline with `why`. Used when a session was found
    /// but connect failed, so cached chats still show instead of a signed-out empty page.
    fn keep_cached_offline(&self, network: Network, why: &str) {
        mark_network_offline(&self.hub, network, why);
    }

    /// Starts an adapter for every account in a network's session.
    fn connect(&self, network: Network, session: &str) -> Result<(), String> {
        for signed in adapters::accounts(network, session)? {
            let account = AccountKey {
                network,
                account: signed.account,
            };
            let named = self.hub.store().set_account(&account, &signed.name, "");
            named.map_err(|err| err.to_string())?;
            let adapter = adapters::start(account.clone(), &signed.session, self.hub.clone())?;
            // Insert and take the catch-ups under one hold, so an ask that arrives in
            // between is either queued here or delivered to the adapter, not dropped.
            let ready = {
                let mut adapters = lock(&self.adapters);
                adapters.insert(account.clone(), adapter);
                self.take_held(&account)
            };
            for command in ready {
                self.command(&account, command);
            }
        }
        Ok(())
    }

    /// History catch-ups queued for `account` before its adapter existed.
    fn take_held(&self, account: &AccountKey) -> Vec<Command> {
        let mut held = lock(&self.held);
        let mut keep = Vec::new();
        let mut ready = Vec::new();
        for (key, command) in held.drain(..) {
            if key == *account {
                ready.push(command);
            } else {
                keep.push((key, command));
            }
        }
        *held = keep;
        ready
    }

    /// Drops the cache of accounts that no longer have a session: signed out while chatd was
    /// down, or renamed by a newer adapter. A scratch directory has no sessions at all and keeps
    /// what was seeded into it.
    fn forget_strangers(&self, settled: &[Network]) {
        if !session::enabled() {
            return;
        }
        let stored = self.hub.store().accounts().unwrap_or_default();
        for account in stored.into_iter().map(|row| row.key) {
            if !settled.contains(&account.network) {
                continue;
            }
            if !lock(&self.adapters).contains_key(&account) {
                self.forget(&account);
            }
        }
    }

    fn forget(&self, account: &AccountKey) {
        lock(&self.held).retain(|(key, _)| key != account);
        self.hub.player.forget(&self.hub, account);
        self.blobs.forget(account);
        if let Err(err) = self.hub.store().forget(account) {
            tracing::warn!("chatd: {}: {err}", account.network.label());
        }
        self.hub.reload_settings();
        self.hub.forget_all_live(account);
    }

    /// Stores a setting and has whatever reads it take it up now.
    fn set(&self, scope: String, key: String, value: Option<String>) -> Response {
        let allowed = match &value {
            Some(value) => settings::valid(&scope, &key, value),
            None => settings::known(&scope, &key),
        };
        if !allowed {
            return error(ErrorCode::Refused, format!("{key} cannot be {value:?} here"));
        }
        if let Err(err) = self.hub.set_setting(&scope, &key, value.clone()) {
            return internal(err);
        }
        match key.as_str() {
            settings::NOTIFY_ABOUT => {
                if let Some(conversation) = settings::conversation_key_of(&scope) {
                    if conversation.network == Network::Tiny {
                        let muted = value.as_deref() == Some("off");
                        if self.hub.store().set_muted(&conversation, muted).unwrap_or(false) {
                            self.hub.changed(Change::Conversation { conversation });
                            self.hub.changed(Change::Status);
                        }
                    }
                }
            }
            settings::KEEP_DAYS => self.hub.prune_soon(),
            settings::CACHE_MB => self.blobs.cap_changed(),
            settings::SOUNDS
            | settings::CUE_JOINS
            | settings::CUE_MINE
            | settings::CUE_CONNECTION
            | settings::CUE_VOLUME
            | settings::VOICE_VOLUME
            | settings::SENSITIVITY
            | settings::PUSH_TO_TALK
            | settings::PTT_RELEASE_MS
            | settings::ECHO_CANCELLATION
            | settings::NOISE_SUPPRESSION
            | settings::AUTO_GAIN
            | settings::INPUT_VOLUME
            | settings::VOICE_QUALITY => self.hub.calls.retune(&self.hub),
            settings::INPUT | settings::OUTPUT => self.hub.calls.redevice(&self.hub),
            // `set_setting` has already moved chatd's own ceiling.
            settings::LOG_LEVEL => self.hub.calls.relevel(),
            _ => {}
        }
        Response::Ok
    }

    /// Reset all (B3). The cache cap and retention go back to their defaults with the rest.
    fn reset_settings(&self) -> Response {
        if let Err(err) = self.hub.reset_settings() {
            return internal(err);
        }
        self.blobs.cap_changed();
        self.hub.prune_soon();
        self.hub.calls.retune(&self.hub);
        self.hub.calls.redevice(&self.hub);
        self.hub.calls.relevel();
        Response::Ok
    }

    /// Bytes on this Mac: the database with its journal, and the cached files.
    fn storage(&self) -> Response {
        let dir = support_dir();
        let size = |name: &str| std::fs::metadata(dir.join(name)).map_or(0, |meta| meta.len());
        let messages = size("chat.sqlite") + size("chat.sqlite-wal") + size("chat.sqlite-shm");
        Response::Storage { messages, files: self.blobs.bytes() }
    }

    /// Clear (D3): the files, then the messages, then every window reads what is left.
    fn clear(&self) -> Response {
        self.blobs.clear();
        if let Err(err) = self.hub.store().clear_cache() {
            return internal(err);
        }
        self.hub.reset();
        Response::Ok
    }

    /// chatd stops: the call is left on the network first, and the answer goes out before the
    /// process does.
    fn shutdown(&self) -> Response {
        // Inside an app the core is the app's: exiting would close the window too. Refused, not
        // answered Ok, so no caller reads a stop that did not happen.
        if self.in_process {
            return error(ErrorCode::Unsupported, "chatd runs inside the app and stops when the app does");
        }
        if let Some(conversation) = self.hub.calls.end(&self.hub) {
            self.command(&conversation.account(), Command::CallLeave(conversation.id));
        }
        let spawned = std::thread::Builder::new().name("shutdown".into()).stack_size(64 * 1024).spawn(|| {
            // Time for this answer to be written and the adapter to say goodbye.
            std::thread::sleep(Duration::from_millis(300));
            tracing::warn!("chatd: asked to stop, exiting");
            std::process::exit(0);
        });
        if spawned.is_err() {
            std::process::exit(0);
        }
        Response::Ok
    }

    fn command(&self, account: &AccountKey, command: Command) {
        let adapters = lock(&self.adapters);
        if let Some(adapter) = adapters.get(account) {
            adapter.command(command);
            return;
        }
        // A catch-up asked while this account's adapter is still starting. Dropping it
        // leaves the stored page on screen: the list refresh that follows is not a
        // message change, so the pane does not ask again.
        if kept_without_adapter(&command) {
            lock(&self.held).push((account.clone(), command));
        }
    }

    /// Asks an account's adapter something and waits for its answer.
    fn ask<T>(&self, account: &AccountKey, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, Refused> {
        let (reply, answer) = channel();
        match lock(&self.adapters).get(account) {
            Some(adapter) => adapter.command(command(reply)),
            None => {
                let why = format!("{} is not signed in", account.network.label());
                return Err(Box::new(error(ErrorCode::Unsupported, why)));
            }
        }
        let refused = match answer.recv_timeout(ANSWER) {
            Ok(Ok(answer)) => return Ok(answer),
            Ok(Err(Refusal::Unsupported)) => error(
                ErrorCode::Unsupported,
                format!("{} cannot do this yet", account.network.label()),
            ),
            Ok(Err(Refusal::Refused(why))) => error(ErrorCode::Refused, why),
            Err(_) => error(ErrorCode::Offline, "the network did not answer"),
        };
        Err(Box::new(refused))
    }

    fn status(&self) -> Response {
        let (call, running) = self.hub.calls.describe();
        let links = self.hub.links();
        let keychain = self.hub.keychain();
        let pending = self.hub.sessions_pending();
        let stored = self.hub.store().accounts().unwrap_or_default();
        let mut accounts: Vec<AccountStatus> = stored
            .into_iter()
            .map(|row| {
                let linked = links.iter().find(|linked| linked.account == row.key);
                let (presence, status) = match row.me.as_str() {
                    "" => (Presence::Unknown, String::new()),
                    me => self.hub.shown(&Key::new(&row.key, me)),
                };
                // An account whose session is still being read, or was refused, is kept on
                // screen as offline with the reason, never dropped as signed out.
                let network = row.key.network;
                let unread_session = match linked {
                    Some(_) => None,
                    None if keychain.denied.contains(&network) => Some(session::STORE_REFUSED),
                    None if keychain.waiting == Some(network) => Some(session::STORE_WAITING),
                    None if pending.contains(&network) => Some(""),
                    None => None,
                };
                AccountStatus {
                    link: match unread_session {
                        Some(_) => Link::Offline,
                        None => linked.map_or(Link::SignedOut, |linked| linked.link),
                    },
                    detail: match unread_session {
                        Some(words) => words.to_owned(),
                        None => linked.map_or_else(String::new, |linked| linked.detail.clone()),
                    },
                    reconnecting: linked.is_some_and(|linked| linked.reconnecting),
                    required: linked.map_or_else(String::new, |linked| linked.required.clone()),
                    key: row.key,
                    name: row.name,
                    me: row.me,
                    attention: row.attention,
                    unread: row.unread,
                    avatar: row.avatar,
                    presence,
                    status,
                    can: linked.map_or_else(Capabilities::default, |linked| linked.can),
                    send_as: linked.map_or_else(Vec::new, |linked| linked.send_as.clone()),
                }
            })
            .collect();
        accounts.retain(|account| account.link != Link::SignedOut || !session::enabled());
        Response::Status {
            networks: Network::ALL
                .into_iter()
                .map(|network| NetworkStatus {
                    network,
                    // The rail trusts this link. A session still in the keychain, or waiting on
                    // it, has no live link yet and would otherwise read as signed out.
                    link: published_link(
                        self.hub.network_link(network),
                        accounts.iter().filter(|account| account.key.network == network).map(|account| account.link),
                    ),
                })
                .collect(),
            accounts,
            media: if running { Media::Running } else { Media::Stopped },
            call,
            ring: self.hub.calls.aside(),
            seq: self.hub.seq(),
            keychain,
            started: self.hub.started,
            session: protocol::log_policy::session_id().to_owned(),
        }
    }

    /// Whether this account's cached chats and messages may be shown. A signed-out account keeps
    /// them on disk so the next sign-in has them, and the window is shown none of them. A
    /// keychain read still in progress is not a sign-out. Scratch directories have no sessions
    /// and keep showing what was seeded.
    fn shows(&self, account: &AccountKey) -> bool {
        if !session::enabled() {
            return true;
        }
        if self.hub.link(account) != Link::SignedOut {
            return true;
        }
        let network = account.network;
        let keychain = self.hub.keychain();
        keychain.denied.contains(&network)
            || keychain.waiting == Some(network)
            || self.hub.sessions_pending().contains(&network)
    }

    /// Accounts whose stored rows must not be listed.
    fn withheld(&self) -> Vec<AccountKey> {
        if !session::enabled() {
            return Vec::new();
        }
        self.hub
            .store()
            .accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.key)
            .filter(|key| !self.shows(key))
            .collect()
    }

    /// A row as the shell is told it: what is stored, and who is in it right now.
    fn row(&self, mut conversation: Conversation) -> Conversation {
        if conversation.kind == ConversationKind::Voice || conversation.call == CallSupport::Native {
            conversation.occupants = self.hub.occupants(&conversation.key);
        }
        conversation
    }

    fn rows(&self, conversations: Vec<Conversation>) -> Vec<Conversation> {
        conversations.into_iter().map(|conversation| self.row(conversation)).collect()
    }

    /// People as the shell is told them: what is stored, and what they are doing now.
    fn with_presence(&self, mut people: Vec<Person>) -> Vec<Person> {
        for person in &mut people {
            let doing = self.hub.doing(&person.key);
            person.presence = doing.presence;
            person.emoji = doing.emoji;
            person.activity = doing.activity.map(Box::new);
            if !doing.status.is_empty() {
                person.status = doing.status;
            }
        }
        people
    }

    fn people(
        &self,
        network: Option<Network>,
        relation: Option<Relation>,
        query: Option<String>,
        offset: u32,
        limit: u32,
    ) -> Response {
        let query = query.map(|query| query.trim().to_owned()).filter(|query| !query.is_empty());
        // An explicit Telegram username lookup still searches cached display names/handles.
        let cached_query = query.as_deref().map(|query| query.trim_start_matches('@'));
        let found = self.hub.store().people(network, relation, cached_query, offset, limit.min(PAGE_LIMIT));
        let mut settled = Vec::new();
        if let (Some(query), 0) = (&query, offset) {
            // The cache answers at once; the network's own search lands as a `People` change,
            // once the typing has paused and only for a query long enough to mean something.
            // One already sent is named on this answer instead, so the shell does not wait
            // for a change that will not come.
            let worth = query.chars().count() >= SEARCH_MIN_CHARS;
            for linked in self.hub.links() {
                let account = linked.account;
                if worth && linked.link == Link::Connected && network.is_none_or(|network| network == account.network) {
                    if self.searches.ask(account.clone(), query) {
                        settled.push(account);
                    }
                }
            }
        }
        match found {
            Ok((people, total)) => Response::People {
                people: self.with_presence(people),
                total,
                settled,
            },
            Err(err) => internal(err),
        }
    }

    fn watch(&self, mut watching: Watching) -> Response {
        watching.people.truncate(WATCH_LIMIT);
        watching.rows.truncate(protocol::ROWS_WATCHED);
        self.fill_previews(&watching.rows);
        let people = {
            let store = self.hub.store();
            let known = watching.people.iter().filter_map(|person| store.person(person).ok().flatten());
            known.collect()
        };
        let typists = watching.conversation.as_ref().map_or_else(Vec::new, |open| self.hub.typists(open));
        // Focus alone is chatd's own business (notifications): the networks are not told again.
        let mut before = (*self.hub.watching()).clone();
        before.focused = watching.focused;
        let for_networks = before != watching;
        if self.hub.watch(watching) && for_networks {
            let watching = self.hub.watching();
            for adapter in lock(&self.adapters).values() {
                adapter.command(Command::Watch(watching.clone()));
            }
        }
        Response::Watching {
            people: self.with_presence(people),
            typists,
        }
    }

    /// Rows on screen with no preview line are asked of their network, each once, paced by
    /// the adapter. Never on Discord: a fetch per DM is the burst that flags accounts there.
    fn fill_previews(&self, rows: &[Key]) {
        let wanted: Vec<Key> = rows.iter().filter(|row| row.network != Network::Discord).cloned().collect();
        if wanted.is_empty() {
            return;
        }
        let missing = self.hub.store().previewless(&wanted).unwrap_or_default();
        let mut by_account: HashMap<AccountKey, Vec<String>> = HashMap::new();
        for row in self.hub.ask_previews(missing) {
            by_account.entry(row.account()).or_default().push(row.id);
        }
        for (account, ids) in by_account {
            if self.hub.link(&account) == Link::Connected {
                self.command(&account, Command::Previews(ids));
            }
        }
    }

    /// A Slack or Teams thread id names its parent, so the conversation can exist before the
    /// network has a reply in it. Opening or sending is what records it. Discord's thread is a
    /// new channel (`StartThread`) and is not derived from the id.
    fn ensure_thread(&self, conversation: &Key) -> rusqlite::Result<()> {
        let mut store = self.hub.store();
        if store.conversation(conversation)?.is_some() {
            return Ok(());
        }
        let Some((parent_id, root_id)) = derivable_thread(conversation) else {
            return Ok(());
        };
        let parent_key = Key { id: parent_id.clone(), ..conversation.clone() };
        let Some(parent) = store.conversation(&parent_key)? else {
            return Ok(());
        };
        if parent.kind == ConversationKind::Thread {
            return Ok(());
        }
        // A Teams chat quotes. Only a channel's reply chain is a thread.
        if conversation.network == Network::Teams
            && !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement)
        {
            return Ok(());
        }
        let title = match conversation.network {
            Network::Teams => {
                let text = store.message(&parent_key, &root_id)?.map(|message| message.text).unwrap_or_default();
                let title = first_line(&text, 40);
                if title.is_empty() { "Replies".to_owned() } else { title }
            }
            Network::Slack => {
                if parent.kind.is_direct() {
                    format!("Thread with {}", parent.title)
                } else {
                    format!("Thread in {}", parent.title)
                }
            }
            _ => return Ok(()),
        };
        let mut row = crate::store::ConversationRow::new(&conversation.id, ConversationKind::Thread, title);
        row.parent = Some(parent_id);
        row.root = Some(root_id);
        row.private = parent.private;
        row.read_only = parent.read_only;
        let account = conversation.account();
        store.upsert_conversation(&account, &row)?;
        drop(store);
        self.hub.changed(Change::Conversation { conversation: conversation.clone() });
        Ok(())
    }

    /// Discord creates the thread channel. The name comes from the message, so the click does
    /// not ask for one. The adapter records the conversation before it answers.
    fn start_thread(&self, conversation: Key, message: String) -> Response {
        if conversation.network != Network::Discord {
            return error(ErrorCode::Unsupported, format!("{} cannot do this yet", conversation.network.label()));
        }
        if let Err(refused) = self.online(&conversation.account()) {
            return *refused;
        }
        let parent = match self.hub.store().conversation(&conversation) {
            Ok(Some(parent)) => parent,
            Ok(None) => return error(ErrorCode::NotFound, "no such conversation"),
            Err(err) => return internal(err),
        };
        if !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement) {
            return error(ErrorCode::Refused, "A thread starts from a channel message.");
        }
        let stored = match self.hub.store().message(&conversation, &message) {
            Ok(Some(stored)) => stored,
            Ok(None) => return error(ErrorCode::NotFound, "no such message"),
            Err(err) => return internal(err),
        };
        if stored.deleted || matches!(stored.kind, MessageKind::System) {
            return error(ErrorCode::Refused, "That message cannot start a thread.");
        }
        let name = thread_name(&stored.text);
        let conversation_id = conversation.id.clone();
        let started = self.ask(&conversation.account(), |reply| Command::StartThread {
            conversation_id,
            message_id: message,
            name,
            reply,
        });
        let id = match started {
            Ok(id) => id,
            Err(refused) => return *refused,
        };
        match self.hub.store().conversation(&Key::new(&conversation.account(), id)) {
            Ok(Some(thread)) => Response::Conversation { conversation: self.row(thread) },
            Ok(None) => error(ErrorCode::Internal, "the thread started, but was not recorded"),
            Err(err) => internal(err),
        }
    }

    /// The topics of a forum, or, asked as the shell asks when it opens one, the topic that
    /// open lands on. The landing's topics are asked of the network once first, and the
    /// answer waits for them, so it is named on what Telegram says now and not on what an
    /// earlier open left in the cache.
    fn topics(&self, forum: Key, offset: u32, limit: u32, landing: bool) -> Response {
        if !self.shows(&forum.account()) {
            // Same refusal as Tree. An empty Conversations page looks like a real empty forum.
            return hidden_list();
        }
        if landing && offset == 0 {
            let _ = self.ask(&forum.account(), |reply| Command::OpenForum { conversation_id: forum.id.clone(), reply });
        }
        let read = if landing { self.hub.store().landing(&forum) } else { self.hub.store().topics(&forum, offset, limit.min(PAGE_LIMIT)) };
        match read {
            Ok((conversations, total)) => Response::Conversations { conversations: self.rows(conversations), total, counts: InboxCounts::default() },
            Err(err) => internal(err),
        }
    }

    /// Earlier wordings already stored for this message. Does not ask the network and does not
    /// keep a wording that was only seen as a live edit.
    fn revisions(&self, conversation: Key, message_id: String) -> Response {
        if !self.shows(&conversation.account()) {
            return hidden_history();
        }
        match self.hub.store().revisions(&conversation, &message_id) {
            Ok(revisions) => Response::Revisions { conversation, message_id, revisions },
            Err(err) => internal(err),
        }
    }

    fn messages(&self, conversation: Key, page: Page, limit: u32) -> Response {
        if !self.shows(&conversation.account()) {
            // Same refusal as Tree and Conversation. An empty Messages page is a successful
            // newest read, and the open pane clears itself with it even when rows stay on disk.
            return hidden_history();
        }
        // A Slack or Teams thread is named by its parent, so the row can exist before any reply.
        if let Err(err) = self.ensure_thread(&conversation) {
            return internal(err);
        }
        let read = {
            let mut store = self.hub.store();
            let swept = store.fail_pending(now() - SEND_SECONDS, "The network did not answer.").and_then(|failed| {
                let aged = store.fail_queued(now() - QUEUE_AGE, QUEUE_LATE)?;
                Ok((failed, aged))
            });
            if let Ok((failed, aged)) = &swept {
                for stuck in failed.iter().chain(aged.iter()).filter(|stuck| **stuck != conversation) {
                    self.hub.changed(Change::Messages { conversation: stuck.clone() });
                }
            }
            swept.and_then(|_| store.messages(&conversation, &page, limit.min(PAGE_LIMIT), now()))
        };
        match read {
            Ok(Some(found)) => {
                // What is stored is answered at once. The network catches it up behind that.
                let account = conversation.account();
                let conversation_id = conversation.id.clone();
                match &page {
                    Page::Newest => self.command(&account, Command::Backfill(conversation_id)),
                    Page::Before { id } => {
                        // `before` excludes that id. The oldest row extends the cache. The anchor
                        // is the page on screen, which a window of blank plaintext still needs.
                        let oldest = found.messages.first().map(|oldest| oldest.id.as_str());
                        for before in before_cursors(id, oldest, window_has_blank(&found.messages), !found.older) {
                            self.command(&account, Command::History { conversation_id: conversation_id.clone(), before: before.to_owned() });
                        }
                    }
                    // The cache cannot see a gap in itself, so the adapter is asked each time
                    // and fetches only what it finds missing.
                    Page::After { id } => {
                        let after = id.clone();
                        self.command(&account, Command::HistoryAfter { conversation_id, after });
                    }
                    Page::Around { id }
                        if around_needs_catchup(
                            found.older,
                            found.newer,
                            found.messages.iter().any(|message| message.id == *id),
                            window_has_blank(&found.messages),
                        ) =>
                    {
                        let around = id.clone();
                        // The list cursor is exclusive, so a page before the anchor misses the
                        // anchor and the newer half. The first message after this window is the
                        // cursor that still includes the window's last row.
                        let last = found.messages.last().map(|message| message.id.clone());
                        let successor = last.and_then(|last| {
                            self.hub
                                .store()
                                .messages(&conversation, &Page::After { id: last }, 1, now())
                                .ok()
                                .flatten()
                                .and_then(|page| page.messages.into_iter().next().map(|message| message.id))
                        });
                        self.command(&account, Command::HistoryAround { conversation_id: conversation_id.clone(), around });
                        if let Some(before) = around_followup(successor.as_deref()).map(str::to_owned) {
                            self.command(&account, Command::History { conversation_id, before });
                        }
                    }
                    _ => {}
                }
                Response::Messages {
                    conversation,
                    messages: found.messages,
                    last_read: found.last_read,
                    older: found.older,
                    newer: found.newer,
                }
            }
            Ok(None) => error(ErrorCode::NotFound, "no such conversation"),
            Err(err) => internal(err),
        }
    }

    fn mark_read(&self, conversation: Key, message_id: String) -> Response {
        // The store is let go before the settings are read: a guard made in a `match` lives
        // to its end, and `Hub::set_setting` takes the two the other way round.
        let marked = self.hub.store().mark_read(&conversation, &message_id);
        match marked {
            Ok(true) => {
                let row = self
                    .hub
                    .store()
                    .conversation(&conversation)
                    .ok()
                    .flatten();
                let is_tiny_channel = conversation.network == Network::Tiny
                    && row.as_ref().is_some_and(|row| row.kind != ConversationKind::Direct);
                let marker = row
                    .and_then(|row| row.last_read)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(message_id);
                let conversation_id = conversation.id.clone();
                // P2: off, the chat is read here and the network is not told, which is what
                // others would see as a receipt. On a Tiny server, a channel read is private
                // (stored for this person's unread count only), so always send it.
                let account = conversation.account();
                if (is_tiny_channel || self.hub.settings().receipts(&account)) && !marker.is_empty() {
                    self.command(&account, Command::MarkRead { conversation_id, message_id: marker });
                }
                self.hub.changed(Change::Conversation { conversation });
                self.hub.changed(Change::Status);
                Response::Ok
            }
            Ok(false) => Response::Ok,
            Err(err) => internal(err),
        }
    }

    /// The user moved their own marker back. `message_id` is the first unread message, or empty
    /// for the newest one stored. The network is told even when receipts are off: this syncs the
    /// user's marker to their other devices, and it is not a receipt the other person sees.
    fn mark_unread(&self, conversation: Key, message_id: String) -> Response {
        match self.hub.store().mark_unread(&conversation, &message_id) {
            Ok(Some((marker, mentions))) => {
                let account = conversation.account();
                self.command(
                    &account,
                    Command::MarkUnread {
                        conversation_id: conversation.id.clone(),
                        message_id: marker,
                        mentions,
                    },
                );
                self.hub.changed(Change::Conversation { conversation });
                self.hub.changed(Change::Status);
                Response::Ok
            }
            Ok(None) => Response::Ok,
            Err(err) => internal(err),
        }
    }

    /// Every badge goes. Discord hears one bulk ack per account; the other networks hear the
    /// read they already know. A message request is read here and not announced. `network`
    /// limits it to one network; `space` to one Space's conversations.
    fn mark_all_read(&self, network: Option<Network>, space: Option<Key>) -> Response {
        let marks = match self.hub.store().unread_marks(network, space.as_ref()) {
            Ok(marks) => marks,
            Err(err) => return internal(err),
        };
        let mut told: HashMap<AccountKey, Vec<(String, String)>> = HashMap::new();
        let mut touched = Vec::new();
        {
            let mut store = self.hub.store();
            for (key, message_id, request) in marks {
                match store.mark_read(&key, &message_id) {
                    Ok(true) => {
                        let marker = store
                            .conversation(&key)
                            .ok()
                            .flatten()
                            .and_then(|row| row.last_read)
                            .filter(|id| !id.is_empty())
                            .unwrap_or(message_id);
                        if !request && !marker.is_empty() {
                            told.entry(key.account()).or_default().push((key.id.clone(), marker));
                        }
                        touched.push(key);
                    }
                    Ok(false) => {}
                    Err(err) => return internal(err),
                }
            }
        }
        for (account, reads) in told {
            if !self.hub.settings().receipts(&account) {
                continue;
            }
            if account.network == Network::Discord {
                for chunk in reads.chunks(50) {
                    self.command(&account, Command::AckBulk { reads: chunk.to_vec() });
                }
            } else {
                for (conversation_id, message_id) in reads {
                    self.command(&account, Command::MarkRead { conversation_id, message_id });
                }
            }
        }
        for conversation in touched {
            self.hub.changed(Change::Conversation { conversation });
        }
        self.hub.changed(Change::Status);
        Response::Ok
    }

    fn forget_conversation(&self, conversation: Key) -> Response {
        match self.hub.store().forget_conversation(&conversation) {
            Ok(true) => {
                self.hub.changed(Change::Messages { conversation: conversation.clone() });
                self.hub.changed(Change::Conversation { conversation });
                self.hub.changed(Change::Status);
                Response::Ok
            }
            Ok(false) => error(ErrorCode::NotFound, "no such conversation"),
            Err(err) => internal(err),
        }
    }

    /// Stores the message as pending and hands it to the adapter. The answer does not wait for
    /// the network; the outcome arrives as a `Messages` change. A text send while the account
    /// is offline is stored and handed off when the link returns.
    fn send(&self, conversation: Key, mut message: Message, reply_to: Option<String>, with: Outgoing) -> Response {
        let account = conversation.account();
        let link = self.hub.link(&account);
        if link != Link::Connected {
            if link == Link::Offline && queueable(&with) {
                return self.queue_offline(conversation, message);
            }
            return error(ErrorCode::Offline, offline_words(&account, &with));
        }
        message.waiting = false;
        if with.poll.is_some() {
            if self.hub.capabilities(&account).polls.is_none() {
                return error(ErrorCode::Unsupported, "Polls are not built for this network yet.");
            }
            if !with.files.is_empty() {
                return error(ErrorCode::Refused, "A poll can't carry a file.");
            }
        }
        if let Err(refused) = self.allowed(&conversation, &with) {
            return *refused;
        }
        if let Err(err) = self.ensure_thread(&conversation) {
            return internal(err);
        }
        let client_id = message.id.clone();
        message.state = SendState::Pending;
        message.sent = now();
        let stored = {
            let mut store = self.hub.store();
            match store.conversation(&conversation) {
                Ok(Some(row)) if row.read_only => {
                    let why = if row.read_only_reason.is_empty() {
                        format!("You can read {} but not post in it.", row.title)
                    } else {
                        row.read_only_reason
                    };
                    return error(ErrorCode::Refused, why);
                }
                Ok(Some(_)) => store.ingest(&conversation, &message, true),
                Ok(None) => return error(ErrorCode::NotFound, "no such conversation"),
                Err(err) => Err(err),
            }
        };
        if let Err(err) = stored {
            return internal(err);
        }
        self.hub.changed(Change::Messages { conversation: conversation.clone() });
        self.hub.changed(Change::Conversation { conversation: conversation.clone() });
        for file in &with.files {
            self.hub.player.stop_file(&self.hub, std::path::Path::new(&file.path));
        }
        let (conversation_id, text) = (conversation.id, message.text.clone());
        let command = match with.is_plain() {
            true => Command::Send { conversation_id, client_id, text, reply_to },
            false => Command::SendWith { conversation_id, client_id, text, reply_to, with: Box::new(with) },
        };
        self.command(&account, command);
        Response::Sent { message }
    }

    /// A text send while the account is offline. Stored pending, not handed to the adapter yet.
    fn queue_offline(&self, conversation: Key, mut message: Message) -> Response {
        let account = conversation.account();
        let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
        let waiting = self.hub.store().queued_count(&account, &client_id).unwrap_or(QUEUE_LIMIT);
        if waiting >= QUEUE_LIMIT {
            return error(
                ErrorCode::Refused,
                format!("Too many messages are waiting for {}. Cancel one, or wait until it is back.", account.network.label()),
            );
        }
        if let Err(err) = self.ensure_thread(&conversation) {
            return internal(err);
        }
        message.state = SendState::Pending;
        message.waiting = true;
        message.sent = now();
        let stored = {
            let mut store = self.hub.store();
            match store.conversation(&conversation) {
                Ok(Some(row)) if row.read_only => {
                    let why = if row.read_only_reason.is_empty() {
                        format!("You can read {} but not post in it.", row.title)
                    } else {
                        row.read_only_reason
                    };
                    return error(ErrorCode::Refused, why);
                }
                Ok(Some(_)) => store.ingest(&conversation, &message, true),
                Ok(None) => return error(ErrorCode::NotFound, "no such conversation"),
                Err(err) => Err(err),
            }
        };
        if let Err(err) = stored {
            return internal(err);
        }
        self.hub.changed(Change::Messages { conversation: conversation.clone() });
        self.hub.changed(Change::Conversation { conversation });
        Response::Sent { message }
    }

    /// Hands this account's offline queue to its adapter, oldest first. One older than a day
    /// fails instead, and stays for Retry. The 60s pending timer starts at this hand-off.
    fn release_queue(&self, account: &AccountKey) {
        if self.hub.link(account) != Link::Connected {
            return;
        }
        let clock = now();
        let aged = match self.hub.store().fail_queued(clock - QUEUE_AGE, QUEUE_LATE) {
            Ok(aged) => aged,
            Err(err) => {
                tracing::warn!("chatd: offline queue: {err}");
                return;
            }
        };
        let queued = match self.hub.store().queued(account) {
            Ok(queued) => queued,
            Err(err) => {
                tracing::warn!("chatd: offline queue: {err}");
                return;
            }
        };
        let mut ready = Vec::with_capacity(queued.len());
        for (conversation, mut message) in queued {
            message.waiting = false;
            message.sent = clock;
            message.state = SendState::Pending;
            if self.hub.store().ingest(&conversation, &message, true).is_err() {
                continue;
            }
            ready.push((conversation, message));
        }
        for conversation in &aged {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
            self.hub.changed(Change::Conversation { conversation: conversation.clone() });
        }
        for (conversation, message) in ready {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
            self.hub.changed(Change::Conversation { conversation: conversation.clone() });
            let conversation_id = conversation.id.clone();
            let reply_to = message.reply_to.as_ref().map(|quoted| quoted.id.clone());
            let client_id = message.client_id.clone().unwrap_or_else(|| message.id.clone());
            self.command(
                account,
                Command::Send {
                    conversation_id,
                    client_id,
                    text: message.text,
                    reply_to,
                },
            );
        }
    }

    /// When this account connects, hand off the text that was written while it was offline.
    fn arm_queue(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.hub.on_connected(move |account| {
            let Some(chat) = weak.upgrade() else { return };
            chat.release_queue(account);
        });
    }

    /// What the account may send, checked before anything is stored: a file too large or too
    /// many is refused here, naming the limit, and the network hears nothing.
    fn allowed(&self, conversation: &Key, with: &Outgoing) -> Result<(), Refused> {
        let account = conversation.account();
        let refuse = |why: String| Err(Box::new(error(ErrorCode::Refused, why)));
        let name = account.network.label();
        let can = self.hub.capabilities(&account);
        if with.files.is_empty() {
            return Ok(());
        }
        let max_file_bytes = self
            .hub
            .store()
            .conversation(conversation)
            .ok()
            .flatten()
            .map_or(can.max_file_bytes, |row| can.max_file_bytes.max(row.max_file_bytes));
        if max_file_bytes == 0 {
            return refuse(format!("Files can't be sent to {name} from tinyclient yet."));
        }
        let most = (can.max_files.max(1) as usize).min(protocol::ATTACHMENTS_LIMIT);
        if with.files.len() > most {
            return refuse(format!("{name} takes at most {most} files in one message."));
        }
        for file in &with.files {
            let named = file_name(file);
            // The same rule the upload reads by: never chatd's own files, a device, a secret, or
            // a link in `outbox/`.
            let real = match crate::blobs::upload::allowed(&file.path) {
                Ok((real, _)) => real,
                Err(why) => return refuse(format!("{named} {why}")),
            };
            match std::fs::metadata(&real) {
                Ok(meta) if meta.is_dir() => return refuse(format!("{named}: folders can't be sent. Zip it first.")),
                Ok(meta) if meta.len() > max_file_bytes => {
                    return refuse(format!("{named} is too big for {name} (limit {}).", megabytes(max_file_bytes)))
                }
                Ok(_) => {}
                Err(_) => return refuse(format!("{named} is no longer there.")),
            }
        }
        Ok(())
    }

    fn new_message(&self, conversation: &Key, client_id: String, text: String, reply_to: Option<&str>, with: &Outgoing) -> Message {
        let store = self.hub.store();
        // Fill who we are from local cache now: the pending row must not wait for the network
        // echo to learn our id, name or avatar. "You" stays only when nothing is cached.
        let mut message = Message::plain(&client_id, "", text, now(), true);
        message.sender = pending_self_sender(&store, conversation);
        // The files are listed at once, from where they are, so the pending row shows them.
        message.attachments = with
            .files
            .iter()
            .take(protocol::ATTACHMENTS_LIMIT)
            .enumerate()
            .map(|(at, file)| {
                let bytes = std::fs::metadata(&file.path).map_or(0, |meta| meta.len());
                let id = format!("upload-{client_id}-{at}");
                let mut attached = protocol::Attachment::new(
                    file_name(file),
                    MediaFile { bytes, path: Some(file.path.clone()), ..MediaFile::new(id, kind_of(&file.path)) },
                );
                attached.spoiler = file.spoiler;
                attached
            })
            .collect();
        message.client_id = Some(client_id);
        message.spans = with.spans.clone();
        message.importance = with.importance;
        let quoted = reply_to.and_then(|id| store.message(conversation, id).ok().flatten());
        message.reply_to = quoted.map(|quoted| protocol::ReplyTo {
            id: quoted.id,
            person: (!quoted.sender.id.is_empty()).then(|| Key::new(&conversation.account(), quoted.sender.id)),
            sender: quoted.sender.name,
            snippet: quoted.text.chars().take(120).collect(),
            deleted: quoted.deleted,
        });
        if let Some(draft) = &with.poll {
            let mut question = draft.question.clone();
            clip(&mut question, 100);
            message.text = format!("Poll: {question}");
            message.poll = Some(Box::new(protocol::Poll::open(question, draft.options.iter().take(10).cloned(), draft.multiple)));
        }
        message
    }

    /// Refuses what needs the network when the account is not connected to it.
    fn online(&self, account: &AccountKey) -> Result<(), Refused> {
        match self.hub.link(account) {
            Link::Connected => Ok(()),
            _ => Err(Box::new(error(ErrorCode::Offline, format!("{} is offline", account.network.label())))),
        }
    }

    /// What reactions a conversation takes, kept five minutes. `None` when the adapter does not
    /// say, or could not ask: then the network alone judges.
    fn reaction_rules(&self, conversation: &Key) -> Result<protocol::ReactionRules, Refused> {
        if let Some(rules) = self.hub.reactions.get(conversation) {
            return Ok(rules);
        }
        let conversation_id = conversation.id.clone();
        let mut rules = self.ask(&conversation.account(), |reply| Command::ReactionRules { conversation_id, reply })?;
        rules.bound();
        self.hub.reactions.keep(conversation.clone(), rules.clone());
        Ok(rules)
    }

    fn reactions(&self, conversation: Key) -> Response {
        match self.reaction_rules(&conversation) {
            Ok(rules) => Response::Reactions { conversation, rules },
            Err(refused) => *refused,
        }
    }

    fn reactors(
        &self,
        conversation: Key,
        message_id: String,
        emoji: String,
        burst: bool,
        after: Option<String>,
        limit: u32,
    ) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let asked = self.ask(&account, |reply| Command::Reactors {
            conversation_id: conversation.id.clone(),
            message_id: message_id.clone(),
            emoji: emoji.clone(),
            burst,
            after,
            limit,
            reply,
        });
        match asked {
            Ok(page) => Response::Reactors {
                conversation,
                message_id,
                emoji,
                reactors: page.reactors,
                more: page.more,
            },
            Err(refused) => *refused,
        }
    }

    /// Our reaction, shown at once and taken back if the network refuses it. One the
    /// conversation's rules refuse is refused here and never sent; one that replaces mine (a
    /// network with one reaction a person) takes mine off at once too. A custom emoji that is
    /// not on the message yet has no name here to draw, so that one waits for the network's word.
    fn react(&self, conversation: Key, message_id: String, emoji: String, on: bool, burst: bool) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        if on && burst && !self.hub.capabilities(&account).burst_reactions {
            return error(ErrorCode::Unsupported, "Super reactions are not available on this account.");
        }
        let ours = |emoji: &str, burst: bool| Reaction {
            emoji: emoji.to_owned(),
            file: None,
            count: if burst { 0 } else { 1 },
            mine: !burst,
            // Ours is one person the store cannot name here; the network's word of the
            // reaction carries the id, and a fresh payload corrects the list.
            who: Vec::new(),
            burst: if burst { 1 } else { 0 },
            mine_burst: burst,
            burst_colors: Vec::new(),
        };
        let message = match self.hub.store().message(&conversation, &message_id) {
            Ok(Some(message)) => message,
            Ok(None) => return error(ErrorCode::NotFound, "no such message"),
            Err(err) => return internal(err),
        };
        let known = self.hub.store().media(&account, std::slice::from_ref(&emoji)).is_ok_and(|known| !known.is_empty());
        // Rules the adapter cannot give leave it to the network, as before there were any.
        let (emoji, replacing) = match (on, self.reaction_rules(&conversation)) {
            (true, Ok(rules)) => match rules.check_kind(&message.reactions, &emoji, known || emoji.starts_with(':'), burst) {
                // Sent as the network spells it, so its own word of it is the same reaction.
                Ok(replacing) => (rules.spelled(&emoji).to_owned(), replacing),
                Err(why) => return error(ErrorCode::Refused, why),
            },
            _ => (emoji, Vec::new()),
        };
        let shown = {
            let mut store = self.hub.store();
            for old in &replacing {
                if let Err(err) = store.react(&conversation, &message_id, &ours(old, false), false) {
                    return internal(err);
                }
            }
            let on_it = message.reactions.iter().any(|have| have.emoji == emoji || have.file.as_ref().is_some_and(|file| file.id == emoji));
            if known && !on_it {
                false
            } else {
                match store.react(&conversation, &message_id, &ours(&emoji, burst), on) {
                    Ok(changed) => changed,
                    Err(err) => return internal(err),
                }
            }
        };
        if shown || !replacing.is_empty() {
            self.hub.changed(Change::Messages { conversation: conversation.clone() });
        }
        let (conversation_id, id) = (conversation.id.clone(), message_id.clone());
        let asked = self.ask(&account, |reply| Command::React {
            conversation_id,
            message_id: id,
            emoji: emoji.clone(),
            on,
            burst,
            reply,
        });
        match asked {
            Ok(()) => Response::Ok,
            Err(refused) => {
                // What the network refused may be what the kept rules allowed: ask again next time.
                self.hub.reactions.forget(&conversation);
                let mut store = self.hub.store();
                let mut undone = shown && store.react(&conversation, &message_id, &ours(&emoji, burst), !on).unwrap_or(false);
                for old in &replacing {
                    let had = message.reactions.iter().find(|have| have.emoji == *old).map(|have| Reaction { count: 1, burst: 0, mine_burst: false, ..have.clone() });
                    undone |= store.react(&conversation, &message_id, &had.unwrap_or_else(|| ours(old, false)), true).unwrap_or(false);
                }
                drop(store);
                if undone {
                    self.hub.changed(Change::Messages { conversation });
                }
                *refused
            }
        }
    }

    /// Edit and delete change nothing here: the answer is the network's, and the message
    /// changes when the adapter reports it.
    fn rewrite(&self, conversation: Key, message_id: String, text: Option<String>, spans: Vec<protocol::Span>, scope: protocol::DeleteScope) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let message = match self.hub.store().message(&conversation, &message_id) {
            Ok(Some(message)) => message,
            Ok(None) => return error(ErrorCode::NotFound, "no such message"),
            Err(err) => return internal(err),
        };
        if message.state != SendState::Sent {
            return error(ErrorCode::Refused, "This message was never sent. Discard it instead.");
        }
        let conversation_id = conversation.id;
        let asked = match text {
            Some(_) if !message.outgoing => return error(ErrorCode::Refused, "Only your own messages can be edited."),
            Some(text) if text.trim().is_empty() => return error(ErrorCode::Refused, "A message cannot be edited to nothing. Delete it instead."),
            Some(text) => self.ask(&account, |reply| Command::Edit {
                conversation_id,
                message_id,
                text,
                spans,
                reply,
            }),
            None => self.ask(&account, |reply| Command::Delete {
                conversation_id,
                message_id,
                scope,
                reply,
            }),
        };
        match asked {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn relate(&self, person: Key, action: RelateAction) -> Response {
        let account = person.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let person_id = person.id;
        match self.ask(&account, |reply| Command::Relate { person_id, action, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn add_friend(&self, account: AccountKey, username: String) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let username = username.trim().to_owned();
        if username.is_empty() {
            return error(ErrorCode::Refused, "Enter a username.");
        }
        match self.ask(&account, |reply| Command::AddFriend { username, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn set_friend_nickname(&self, person: Key, nickname: Option<String>) -> Response {
        let account = person.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let nickname = nickname.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        match self.ask(&account, |reply| Command::SetFriendNickname {
            person_id: person.id,
            nickname,
            reply,
        }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn invite_preview(&self, account: AccountKey, code: String, event_id: Option<String>) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let code = code.trim().to_owned();
        if code.is_empty() {
            return error(ErrorCode::Refused, "Enter an invite link or code.");
        }
        match self.ask(&account, |reply| Command::InvitePreview { code, event_id, reply }) {
            Ok(preview) => Response::InvitePreview { preview },
            Err(refused) => *refused,
        }
    }

    fn event_preview(&self, space: Key, event_id: String) -> Response {
        let account = space.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let event_id = event_id.trim().to_owned();
        if event_id.is_empty() {
            return error(ErrorCode::Refused, "Missing event id.");
        }
        match self.ask(&account, |reply| Command::EventPreview { space_id: space.id, event_id, reply }) {
            Ok(event) => Response::EventPreview { event },
            Err(refused) => *refused,
        }
    }

    fn event_interest(&self, space: Key, event_id: String, interested: bool) -> Response {
        let account = space.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let event_id = event_id.trim().to_owned();
        if event_id.is_empty() {
            return error(ErrorCode::Refused, "Missing event id.");
        }
        match self.ask(&account, |reply| Command::EventInterest { space_id: space.id, event_id, interested, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn invite_join(&self, account: AccountKey, code: String) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let code = code.trim().to_owned();
        if code.is_empty() {
            return error(ErrorCode::Refused, "Enter an invite link or code.");
        }
        match self.ask(&account, |reply| Command::InviteJoin { code, reply }) {
            Ok((space_id, channel_id)) => Response::InviteJoined {
                space: space_id.map(|id| Key::new(&account, id)),
                conversation: channel_id.map(|id| Key::new(&account, id)),
            },
            Err(refused) => *refused,
        }
    }

    fn leave_space(&self, space: Key) -> Response {
        let account = space.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::LeaveSpace { space_id: space.id.clone(), reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn report(
        &self,
        target: protocol::ReportTarget,
        reason: protocol::ReportReason,
        mut note: String,
        block: bool,
    ) -> Response {
        let account = target.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        clip(&mut note, 1000);
        match self.ask(&account, |reply| Command::Report { target, reason, note, block, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn report_reasons(&self, target: protocol::ReportTarget) -> Response {
        let account = target.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::ReportReasons { target, reply }) {
            Ok(reasons) if !reasons.is_empty() => Response::ReportReasons { reasons },
            Ok(_) => Response::ReportReasons { reasons: protocol::ReportReason::ALL.to_vec() },
            Err(refused) if matches!(&*refused, Response::Error { code: ErrorCode::Unsupported, .. }) => {
                Response::ReportReasons { reasons: protocol::ReportReason::ALL.to_vec() }
            }
            Err(refused) => *refused,
        }
    }

    /// Our own presence, shown at once and put back if the network refuses it.
    fn set_presence(&self, account: AccountKey, presence: Presence, status: Option<String>, status_emoji: Option<String>, clear_at: Option<i64>) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let stored = self.hub.store().accounts().unwrap_or_default();
        let me = stored.into_iter().find(|row| row.key == account).map(|row| row.me).filter(|me| !me.is_empty());
        let me = me.map(|me| Key::new(&account, me));
        let status = status.map(|mut status| {
            clip(&mut status, 128);
            status
        });
        let status_emoji = status_emoji.map(|mut emoji| {
            clip(&mut emoji, 64);
            emoji
        });
        let before = me.as_ref().map(|me| self.hub.shown(me));
        if let (Some(me), Some((_, old))) = (&me, &before) {
            self.hub.set_presence(me.clone(), presence, status.as_deref().unwrap_or(old), true);
            self.hub.changed(Change::Status);
        }
        match self.ask(&account, |reply| Command::SetPresence { presence, status, status_emoji, clear_at, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => {
                if let (Some(me), Some((presence, status))) = (me, before) {
                    self.hub.set_presence(me, presence, &status, true);
                    self.hub.changed(Change::Status);
                }
                *refused
            }
        }
    }

    /// Our own profile. `None` leaves that part. Signal uses its encrypted profile-status wire;
    /// other networks dispatch `Command::SetProfile` with unsupported defaults.
    fn set_profile(
        &self,
        account: AccountKey,
        name: Option<String>,
        about: Option<String>,
        pronouns: Option<String>,
        emoji: Option<String>,
        username: Option<String>,
        photo: bool,
    ) -> Response {
        if account.network == Network::Signal {
            if photo {
                return error(ErrorCode::Refused, adapters::signal::PHOTO_ON_PHONE);
            }
            if username.is_some() {
                return error(ErrorCode::Refused, adapters::signal::USERNAME_ON_PHONE);
            }
            if let Err(refused) = self.online(&account) {
                return *refused;
            }
            let status = adapters::signal::profile_status(name.as_deref(), about.as_deref(), emoji.as_deref());
            return match self.ask(&account, |reply| Command::SetPresence { presence: Presence::Online, status: Some(status), status_emoji: None, clear_at: None, reply }) {
                Ok(()) => {
                    self.forget_own_profile(&account, None);
                    Response::Ok
                }
                Err(refused) => *refused,
            };
        }
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::SetProfile { name, about, pronouns, reply }) {
            Ok(()) => {
                self.forget_own_profile(&account, None);
                Response::Ok
            }
            Err(refused) => *refused,
        }
    }

    /// Sets our own avatar from a local file path. Maps Tiny onto its existing avatar upload.
    fn set_avatar(&self, account: AccountKey, path: String) -> Response {
        if account.network == Network::Tiny {
            let response = adapters::tiny::manage::user_avatar(account.clone(), (!path.is_empty()).then_some(path));
            if matches!(response, Response::Ok) {
                self.forget_own_profile(&account, None);
            }
            return response;
        }
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::SetAvatar { path, reply }) {
            Ok(()) => {
                self.forget_own_profile(&account, None);
                Response::Ok
            }
            Err(refused) => *refused,
        }
    }

    /// Sets or clears our per-space nickname.
    fn set_nickname(&self, space: Key, nickname: Option<String>) -> Response {
        let account = space.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let space_id = space.id.clone();
        let nickname = nickname.and_then(|n| {
            let trimmed = n.trim().to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        match self.ask(&account, |reply| Command::SetNickname { space_id, nickname, reply }) {
            Ok(()) => {
                self.forget_own_profile(&account, Some(space));
                Response::Ok
            }
            Err(refused) => *refused,
        }
    }

    fn forget_own_profile(&self, account: &AccountKey, space: Option<Key>) {
        if let Some(me) = self.hub.store().accounts().ok().and_then(|rows| {
            rows.into_iter().find(|row| row.key == *account).map(|row| row.me).filter(|me| !me.is_empty())
        }) {
            self.hub.profiles.forget(&(Key::new(account, me), space));
        }
    }

    /// A vote, a retraction, or ending a poll. Dispatched to the account's adapter via `Command::Vote`.
    fn poll(&self, conversation: Key, message_id: String, votes: Option<Vec<u32>>) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let votes = votes.map(|votes| votes.into_iter().take(protocol::POLL_OPTIONS).collect::<Vec<_>>());
        let conversation_id = conversation.id;
        match self.ask(&account, |reply| Command::Vote { conversation_id, message_id, votes, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    /// One person's card: asked of the network once, kept five minutes. Opened from a Space,
    /// the adapter is asked for what is said of them there, and failing that for the card alone.
    fn profile(&self, person: Key, space: Option<Key>) -> Response {
        let known = match self.hub.store().person(&person) {
            Ok(Some(known)) => known,
            Ok(None) => return error(ErrorCode::NotFound, "no such person"),
            Err(err) => return internal(err),
        };
        let cached = (person.clone(), space.clone());
        if let Some(mut profile) = self.hub.profiles.get(&cached) {
            profile.person = self.with_presence(vec![known]).remove(0);
            return Response::Profile { profile: Box::new(profile) };
        }
        let account = person.account();
        let space_id = space.filter(|space| space.account() == account).map(|space| space.id);
        let asked = match space_id {
            Some(space_id) => {
                let person_id = person.id.clone();
                match self.ask(&account, |reply| Command::ProfileIn { person_id, space_id, reply }) {
                    Err(refused) if matches!(*refused, Response::Error { code: ErrorCode::Unsupported, .. }) => None,
                    asked => Some(asked),
                }
            }
            None => None,
        };
        let asked = asked.unwrap_or_else(|| {
            let person_id = person.id.clone();
            self.ask(&account, |reply| Command::Profile { person_id, reply })
        });
        let about = match asked {
            Ok(about) => about,
            Err(refused) => return *refused,
        };
        let profile = typed_profile(self.with_presence(vec![known]).remove(0), about);
        self.hub.profiles.keep(cached, profile.clone());
        Response::Profile { profile: Box::new(profile) }
    }

    /// A conversation's info: its row, and what the network says beyond it, kept five minutes.
    fn conversation_info(&self, conversation: Key) -> Response {
        let row = match self.hub.store().conversation(&conversation) {
            Ok(Some(row)) => self.row(row),
            Ok(None) => return error(ErrorCode::NotFound, "no such conversation"),
            Err(err) => return internal(err),
        };
        if let Some(mut info) = self.hub.infos.get(&conversation) {
            info.conversation = row;
            return Response::ConversationInfo { info: Box::new(info) };
        }
        let conversation_id = conversation.id.clone();
        let about = match self.ask(&conversation.account(), |reply| Command::ConversationInfo { conversation_id, reply }) {
            Ok(about) => about,
            Err(refused) => return *refused,
        };
        let mut info = ConversationInfo {
            disappearing: about.disappearing.or(row.disappearing),
            conversation: row,
            description: about.description,
            members: about.members,
            online: about.online,
            created: about.created,
            creator: about.creator,
            notify: about.notify,
            pinned: about.pinned,
            external: about.external,
            link: about.link,
            bookmarks: about.bookmarks,
        };
        clip(&mut info.description, 4_000);
        clip(&mut info.creator, 100);
        clip(&mut info.external, 100);
        clip(&mut info.link, 300);
        self.hub.infos.keep(conversation, info.clone());
        Response::ConversationInfo { info: Box::new(info) }
    }

    fn space_info(&self, space: Key) -> Response {
        let stored = self.hub.store().spaces(Some(space.network)).map(|spaces| spaces.into_iter().find(|known| known.key == space));
        let known = match stored {
            Ok(Some(known)) => known,
            Ok(None) => return error(ErrorCode::NotFound, "no such space"),
            Err(err) => return internal(err),
        };
        if let Some(mut info) = self.hub.space_infos.get(&space) {
            info.space = known;
            return Response::SpaceInfo { info: Box::new(info) };
        }
        let space_id = space.id.clone();
        let about = match self.ask(&space.account(), |reply| Command::SpaceInfo { space_id, reply }) {
            Ok(about) => about,
            Err(refused) => return *refused,
        };
        let mut info = SpaceInfo {
            space: known,
            description: about.description,
            members: about.members,
            online: about.online,
            nickname: about.nickname,
            roles: about.roles,
            notify: about.notify,
        };
        clip(&mut info.description, 4_000);
        clip(&mut info.nickname, 100);
        info.roles.truncate(protocol::PROFILE_ROLES);
        info.roles.iter_mut().for_each(|role| clip(role, 100));
        self.hub.space_infos.keep(space, info.clone());
        Response::SpaceInfo { info: Box::new(info) }
    }

    /// Something the user did to a conversation on the network: mute, leave, close. The answer
    /// is the network's; the row changes when the adapter reports it.
    fn act(&self, conversation: &Key, command: impl FnOnce(String, Reply<()>) -> Command) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let conversation_id = conversation.id.clone();
        match self.ask(&account, |reply| command(conversation_id, reply)) {
            Ok(()) => {
                self.hub.infos.forget(conversation);
                Response::Ok
            }
            Err(refused) => *refused,
        }
    }

    fn pinned(&self, conversation: Key) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let conversation_id = conversation.id.clone();
        match self.ask(&account, |reply| Command::Pinned { conversation_id, reply }) {
            Ok(mut messages) => {
                messages.truncate(protocol::PINNED_LIMIT);
                messages.iter_mut().for_each(Message::bound);
                Response::Pinned { conversation, messages }
            }
            Err(refused) => *refused,
        }
    }

    fn export_cache(
        &self,
        scope: protocol::ExportScope,
        destination: String,
        since: Option<i64>,
        until: Option<i64>,
        include_files: bool,
    ) -> Response {
        let dest = destination.trim();
        if dest.is_empty() {
            return error(ErrorCode::Refused, "Choose a destination folder for the export.");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let store = self.hub.store();
        match crate::export::run(&store, &scope, std::path::Path::new(dest), since, until, include_files, now) {
            Ok(summary) => Response::Exported {
                path: summary.path.to_string_lossy().into_owned(),
                conversations: summary.conversations,
                messages: summary.messages,
                files: summary.files,
                earliest: summary.earliest,
                latest: summary.latest,
            },
            Err(why) => error(ErrorCode::Refused, why),
        }
    }

    fn forward(&self, from: Key, message_ids: Vec<String>, to: Vec<Key>, note: String) -> Response {
        if message_ids.is_empty() || to.is_empty() {
            return error(ErrorCode::Refused, "Choose at least one message and one conversation.");
        }
        if to.len() > protocol::FORWARD_LIMIT {
            return error(
                ErrorCode::Refused,
                format!("You can forward to at most {} conversations at once.", protocol::FORWARD_LIMIT),
            );
        }
        let account = from.account();
        if to.iter().any(|target| target.account() != account) {
            return error(
                ErrorCode::Refused,
                "Forwarding only works within the same account. Use Copy to… for another account.",
            );
        }
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let to_ids: Vec<String> = to.iter().map(|k| k.id.clone()).collect();
        let from_id = from.id.clone();
        match self.ask(&account, |reply| Command::Forward {
            from_conversation_id: from_id,
            message_ids,
            to_conversation_ids: to_ids,
            note,
            reply,
        }) {
            Ok(outcomes) => {
                let results = outcomes
                    .into_iter()
                    .map(|(id, res)| protocol::ForwardOutcome {
                        conversation: Key { id, ..from.clone() },
                        ok: res.is_ok(),
                        error: res.err(),
                    })
                    .collect();
                Response::Forwarded { results }
            }
            Err(refused) => *refused,
        }
    }

    fn resolve_link(&self, url: &str) -> Response {
        let Some(parsed) = protocol::link::parse_network_link(url) else {
            return Response::Resolved { conversation: None, message_id: None, title: String::new() };
        };
        match self.hub.store().resolve_link(&parsed) {
            Ok(Some(conv)) => Response::Resolved {
                conversation: Some(conv.key),
                message_id: parsed.message_id,
                title: conv.title,
            },
            Ok(None) => Response::Resolved { conversation: None, message_id: None, title: String::new() },
            Err(err) => internal(err),
        }
    }

    fn source(&self, conversation: Key, message_id: String) -> Response {
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let conversation_id = conversation.id;
        match self.ask(&account, |reply| Command::Source { conversation_id, message_id, reply }) {
            Ok(text) => Response::Source { text },
            Err(refused) => *refused,
        }
    }

    fn catalog(&self, account: AccountKey, kind: CatalogKind, space: Option<Key>, query: String, cursor: Option<String>) -> Response {
        let mut query = query.trim().to_owned();
        clip(&mut query, 100);
        let asked = (account.clone(), kind, space.clone(), query.clone(), cursor.clone());
        if let Some((items, cursor)) = self.hub.catalogs.get(&asked) {
            return Response::Catalog { items, cursor };
        }
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let space_id = space.filter(|space| space.account() == account).map(|space| space.id);
        match self.ask(&account, |reply| Command::Catalog { kind, space_id, query, cursor, reply }) {
            Ok(mut page) => {
                page.items.truncate(protocol::CATALOG_LIMIT);
                for item in &mut page.items {
                    clip(&mut item.name, 100);
                    clip(&mut item.pack, 100);
                }
                self.hub.catalogs.keep(asked, (page.items.clone(), page.cursor.clone()));
                Response::Catalog { items: page.items, cursor: page.cursor }
            }
            Err(refused) => *refused,
        }
    }

    /// Voice channels and calls with a friend in them now, the fullest first.
    fn in_voice(&self, network: Option<Network>) -> Response {
        let mut occupied = self.hub.occupied();
        occupied.retain(|(conversation, _)| network.is_none_or(|network| conversation.network == network));
        occupied.retain(|(conversation, _)| self.shows(&conversation.account()));
        occupied.sort_by_key(|(conversation, occupants)| (std::cmp::Reverse(occupants.total), conversation.id.clone()));
        let mut rows = Vec::new();
        {
            let store = self.hub.store();
            for (conversation, occupants) in occupied {
                let Ok(Some(row)) = store.conversation(&conversation) else {
                    continue;
                };
                let friend = occupants.people.iter().any(|occupant| {
                    store.person(&occupant.person).ok().flatten().is_some_and(|person| person.relation == Relation::Friend)
                });
                if friend || row.kind.is_direct() {
                    rows.push(row);
                }
                if rows.len() >= 50 {
                    break;
                }
            }
        }
        let total = rows.len() as u32;
        Response::Conversations { conversations: self.rows(rows), total, counts: InboxCounts::default() }
    }

    /// Who could I talk to now: in a call, around, talked to lately. Bots never.
    fn around(&self, network: Option<Network>) -> Response {
        let read = {
            let store = self.hub.store();
            let owned = store.owned(network, 2_000);
            let talked = store.talked_to(network, now() - crate::store::RECENT_SECONDS, 500);
            let requests = store.incoming(network);
            owned.and_then(|owned| Ok((owned, talked?, requests?)))
        };
        let (owned, talked, requests) = match read {
            Ok(read) => read,
            Err(err) => return internal(err),
        };
        let (mut in_call, mut online, mut recent) = (Vec::new(), Vec::new(), Vec::new());
        let mut seen = std::collections::HashSet::new();
        let talked_ids: std::collections::HashSet<Key> = talked.iter().map(|person| person.key.clone()).collect();
        for person in self.with_presence(owned.into_iter().chain(talked).collect()) {
            if !self.shows(&person.key.account()) || !seen.insert(person.key.clone()) {
                continue;
            }
            if person.activity.as_ref().is_some_and(|activity| activity.kind.in_call()) {
                in_call.push(person);
            } else if person.presence.is_around() {
                online.push(person);
            } else if talked_ids.contains(&person.key) {
                recent.push(person);
            }
        }
        for section in [&mut in_call, &mut online, &mut recent] {
            section.sort_by_cached_key(|person| person.name.to_lowercase());
            section.truncate(protocol::AROUND_LIMIT);
        }
        Response::Around { requests, in_call, online, recent }
    }

    /// What was shared: from the cache, saying it is only that; or, because the user asked,
    /// from the network's own search.
    fn shared(&self, conversation: Key, kind: SharedKind, before: Option<String>, limit: u32, search: bool) -> Response {
        if !self.shows(&conversation.account()) {
            return error(ErrorCode::NotFound, "signed out");
        }
        let limit = limit.min(PAGE_LIMIT);
        if !search {
            return match self.hub.store().shared(&conversation, kind, before.as_deref(), limit) {
                Ok(Some((items, more))) => Response::Shared { conversation, items, more, complete: false, total: None },
                Ok(None) => error(ErrorCode::NotFound, "no such conversation"),
                Err(err) => internal(err),
            };
        }
        let account = conversation.account();
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let conversation_id = conversation.id.clone();
        match self.ask(&account, |reply| Command::SearchShared { conversation_id, kind, before, limit, reply }) {
            Ok(mut page) => {
                page.items.truncate(limit as usize);
                Response::Shared { conversation, items: page.items, more: page.more, complete: true, total: page.total }
            }
            Err(refused) => *refused,
        }
    }

    fn call_history(&self, account: AccountKey, missed: bool, before: Option<String>, limit: u32) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        let limit = limit.clamp(1, PAGE_LIMIT);
        match self.ask(&account, |reply| Command::CallHistory { missed, before, limit, reply }) {
            Ok(mut page) => {
                page.entries.truncate(limit as usize);
                Response::CallHistory { entries: page.entries, next: page.next, more: page.more }
            }
            Err(refused) => *refused,
        }
    }

    fn delete_call_entries(&self, account: AccountKey, message_ids: Vec<String>, revoke: bool) -> Response {
        if message_ids.is_empty() {
            return Response::Ok;
        }
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::DeleteCallEntries { message_ids, revoke, reply }) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    fn get_privacy(&self, account: AccountKey, key: protocol::PrivacyKey) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::GetPrivacy { key, reply }) {
            Ok(policy) => Response::Privacy { key, policy },
            Err(refused) => *refused,
        }
    }

    fn set_privacy(&self, account: AccountKey, key: protocol::PrivacyKey, policy: protocol::PrivacyPolicy) -> Response {
        if let Err(refused) = self.online(&account) {
            return *refused;
        }
        match self.ask(&account, |reply| Command::SetPrivacy { key, policy, reply }) {
            Ok(policy) => Response::Privacy { key, policy },
            Err(refused) => *refused,
        }
    }

    /// Drops an unsent row locally. A queued send is cancelled this way: the adapter never had it.
    fn drop_unsent(&self, conversation: Key, client_id: &str, message: &Message) -> Response {
        match self.hub.store().delete_message(&conversation, client_id) {
            Ok(_) => {
                crate::blobs::upload::discard(&outgoing_files(message));
                self.hub.changed(Change::Messages { conversation });
                Response::Ok
            }
            Err(err) => internal(err),
        }
    }

    /// The unsent message with this client id, if it is one the user may retry or discard.
    fn unsent(&self, conversation: &Key, client_id: &str) -> Result<Message, Refused> {
        match self.hub.store().message(conversation, client_id) {
            Ok(Some(message)) if message.state != SendState::Sent => Ok(message),
            Ok(_) => Err(Box::new(error(ErrorCode::NotFound, "no such unsent message"))),
            Err(err) => Err(Box::new(internal(err))),
        }
    }

    /// Shows the network's sign-in and waits for the user to finish it.
    fn sign_in(&self, network: Network, at: Option<(i32, i32)>) -> Response {
        if !adapters::supported(network) {
            return error(
                ErrorCode::Unsupported,
                format!("{} is not built yet", network.label()),
            );
        }
        // Signal links this computer from the phone, in the sheet the shell draws itself
        // (`SignInBegin`); it has no sign-in page for a window to show.
        if network == Network::Signal {
            return error(ErrorCode::Unsupported, "Signal signs in by linking from your phone, not in a window.");
        }
        // Tiny signs in through the sheet (`SignInBegin`), never the browser helper.
        if network == Network::Tiny {
            return error(ErrorCode::Unsupported, "A Tiny server signs in in the sheet, not in a window.");
        }
        // A scratch directory signs nobody in. A test may still name a helper of its own, to
        // see what it is run with and how its answer is told; its session is never kept.
        // A phone shows the page in its own web view (`protocol::web_sign_in`) and the same
        // logic as the helper's runs here (`login::web`); a desktop runs the helper.
        let web = protocol::web_sign_in::provider();
        if !session::enabled() && std::env::var_os("TINYCLIENT_LOGIN").is_none() && web.is_none() {
            return error(ErrorCode::SignIn, "sign-in is off while TINYCLIENT_DIR is set");
        }
        let session = match web {
            Some(web) => match login::web::sign_in(&*web, &network.label().to_lowercase()) {
                Ok(session) => session,
                Err(code) => return sign_in_ended(network, Some(code)),
            },
            None => match self.sign_in_helper(network, at) {
                Ok(session) => session,
                Err(answer) => return *answer,
            },
        };
        // The new session replaces every adapter of the network: an expired account comes back,
        // and the accounts that were fine reconnect once.
        let started = session::save(network, &session).and_then(|()| {
            lock(&self.adapters).retain(|account, _| account.network != network);
            self.connect(network, &session)
        });
        match started {
            Ok(()) => self.status(),
            Err(why) => error(ErrorCode::SignIn, why),
        }
    }

    /// The desktop's sign-in: the helper window (`tinyclient-login`), and the session it prints.
    fn sign_in_helper(&self, network: Network, at: Option<(i32, i32)>) -> Result<String, Refused> {
        let helper = login_path().and_then(|path| {
            let mut helper = std::process::Command::new(path);
            protocol::log_policy::share_session(&mut helper);
            protocol::launch::no_console(&mut helper).arg(network.label().to_lowercase());
            if let Some((x, y)) = at {
                helper.args(["--at", &format!("{x},{y}")]);
            }
            helper.stderr(std::process::Stdio::inherit()).output()
        });
        match helper {
            Ok(output) if output.status.success() => Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned()),
            Ok(output) => Err(Box::new(sign_in_ended(network, output.status.code()))),
            Err(err) => Err(Box::new(error(ErrorCode::SignIn, format!("sign-in could not start: {err}")))),
        }
    }

    /// A sign-in the shell draws itself: begins it, or passes on what the user did, and answers
    /// with the step to draw. Whenever it ends signed in, `adopt` keeps the session and starts
    /// the account; that may be long after this answered (a QR code scanned later).
    fn sign_in_step(&self, network: Network, input: Option<protocol::SignInInput>) -> Response {
        let (hub, adapters) = (self.hub.clone(), self.adapters.clone());
        let adopt = move |session: &str| adopt(&hub, &adapters, network, session);
        let step = match network {
            Network::Telegram => adapters::telegram::signin::step(&self.hub, input, Box::new(adopt)),
            Network::Signal => adapters::signal::signin::step(&self.hub, input, Box::new(adopt)),
            Network::Messages => adapters::messages::signin::step(&self.hub, input, Box::new(adopt)),
            Network::Tiny => adapters::tiny::signin::step(&self.hub, input, Box::new(adopt)),
            _ => Err(format!("{} signs in in its own window.", network.label())),
        };
        match step {
            Ok(step) => Response::SignIn { network, step },
            Err(why) => error(ErrorCode::Unsupported, why),
        }
    }

    fn sign_out(&self, account: AccountKey, erase: bool) -> Response {
        // Taken out of the map first, so nothing else is asked of it; kept until it has had
        // its time to end the session on the network, since dropping it stops its threads.
        let adapter = lock(&self.adapters).remove(&account);
        if let Some(adapter) = adapter {
            let (reply, done) = channel();
            adapter.command(Command::SignOut(reply));
            // Answered, or dropped unanswered by an adapter with nothing to end, or too slow.
            let _ = done.recv_timeout(SIGN_OUT);
        }
        let stored = match session::read(account.network) {
            session::Stored::Found(session) => Some(session),
            session::Stored::Absent => None,
            // Forgetting the item now would sign out every other account it holds.
            session::Stored::Unreadable(why) => return error(ErrorCode::Internal, format!("The session could not be read from {}: {why}", session::STORE)),
        };
        if account.network == Network::Tiny {
            if erase {
                adapters::tiny::erase_history(&account.account);
            } else if let Some(session) = &stored {
                // The key of what this device opened outlives the session that held it. Kept or
                // not, the sign-out goes on: the history then stays sealed and unread.
                if let Err(why) = adapters::tiny::keep_history(session, &account.account) {
                    tracing::warn!("chatd: Tiny: the history key could not be kept: {why}");
                }
            }
        }
        let left = stored.and_then(|session| adapters::without(account.network, &session, &account.account));
        match left {
            Some(session) => {
                if let Err(why) = session::save(account.network, &session) {
                    if session::enabled() {
                        return error(ErrorCode::Internal, why);
                    }
                }
            }
            None => session::forget(account.network),
        }
        self.forget(&account);
        self.hub.set_link(&account, Link::SignedOut, "");
        self.hub.changed(Change::Status);
        self.status()
    }

    /// Wipes local session, history, and adapter state after `DELETE /v1/me` succeeded on the
    /// server, without calling `DELETE /v1/session` or keeping history.
    fn forget_deleted_tiny(&self, account: AccountKey) -> Response {
        let _ = lock(&self.adapters).remove(&account);
        let stored = match session::read(account.network) {
            session::Stored::Found(session) => Some(session),
            session::Stored::Absent => None,
            session::Stored::Unreadable(why) => {
                return error(ErrorCode::Internal, format!("The session could not be read from {}: {why}", session::STORE));
            }
        };
        adapters::tiny::erase_history(&account.account);
        let left = stored.and_then(|session| adapters::without(account.network, &session, &account.account));
        match left {
            Some(session) => {
                if let Err(why) = session::save(account.network, &session) {
                    if session::enabled() {
                        return error(ErrorCode::Internal, why);
                    }
                }
            }
            None => session::forget(account.network),
        }
        self.forget(&account);
        self.hub.set_link(&account, Link::SignedOut, "");
        self.hub.changed(Change::Status);
        self.status()
    }

    /// A seat in a Tiny room's call, or someone else's taken away, over that server's live
    /// socket. The answer is the server's.
    fn tiny_call(&self, account: &AccountKey, may: &str, ids: &[String], ask: impl FnOnce(Reply<()>) -> adapters::tiny::live::Ask) -> Response {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        if let Err(answer) = adapters::tiny::call_allowed(account, may, &ids) {
            return answer;
        }
        self.tiny_ask(account, ask)
    }

    /// Server mute, deafen or move, on a server with voice moderation, over its live socket.
    fn tiny_moderate(&self, account: &AccountKey, needs: &[&str], ids: &[&str], ask: impl FnOnce(Reply<()>) -> adapters::tiny::live::Ask) -> Response {
        let feature = protocol::TINY_FEATURE_VOICE_MODERATION;
        if let Err(answer) = adapters::tiny::manage::call_allowed(account, feature, needs, ids) {
            return answer;
        }
        self.tiny_ask(account, ask)
    }

    fn tiny_ask(&self, account: &AccountKey, ask: impl FnOnce(Reply<()>) -> adapters::tiny::live::Ask) -> Response {
        if let Err(refused) = self.online(account) {
            return *refused;
        }
        match self.ask(account, |reply| Command::Tiny(ask(reply))) {
            Ok(()) => Response::Ok,
            Err(refused) => *refused,
        }
    }

    /// A seat in a Tiny room. With audio, chatd's call holds it, and the server hears of the
    /// mute or deafen the call joined with (V4) once the seat is taken.
    fn tiny_join(&self, account: AccountKey, room_id: String) -> Response {
        let ids = [room_id.clone()];
        let room = Key::new(&account, &room_id);
        self.hub.calls.clicked(&room);
        let answer = self.tiny_call(&account, "community.calls.join", &ids, |reply| adapters::tiny::live::Ask::Join { room_id, reply });
        if answer == Response::Ok {
            if let Some(call) = self.hub.calls.describe().0.filter(|call| call.conversation == room && (call.muted || call.deafened)) {
                self.command(&account, Command::CallAudio { muted: call.muted, deafened: call.deafened });
            }
        }
        answer
    }

    /// Only a conversation tinyclient can carry a call for starts one. Anything else is refused
    /// before mediad is spawned or the network hears a thing.
    fn call_start(&self, conversation: Key) -> Response {
        // A Tiny room is a seat on the live socket. A direct message rings, and media starts
        // only once the other person accepts.
        if conversation.network == Network::Tiny {
            return self.tiny_call_start(conversation);
        }
        match self.hub.store().conversation(&conversation) {
            Ok(Some(row)) if row.call == CallSupport::Native => {}
            Ok(Some(row)) if row.call == CallSupport::Handoff => {
                return error(ErrorCode::Unsupported, format!("{} calls open in its own app.", conversation.network.label()))
            }
            Ok(Some(row)) if row.call == CallSupport::Restricted => {
                return error(ErrorCode::Refused, "This person's privacy settings don't allow calls.")
            }
            Ok(Some(_)) => return error(ErrorCode::Unsupported, "This conversation has no calls tinyclient can join."),
            Ok(None) => return error(ErrorCode::NotFound, "no such conversation"),
            Err(err) => return internal(err),
        }
        if let Err(err) = self.hub.calls.start(&self.hub, conversation.clone()) {
            return call_error(err);
        }
        self.join(conversation);
        self.status()
    }

    /// A Tiny voice room is joined as before. A direct message the server can ring is a ring,
    /// with no mediad until it is accepted. An id this client has never stored is still joined
    /// as a room, which is how a room call is started.
    fn tiny_call_start(&self, conversation: Key) -> Response {
        let row = match self.hub.store().conversation(&conversation) {
            Ok(row) => row,
            Err(err) => return internal(err),
        };
        let direct = row
            .as_ref()
            .is_some_and(|row| row.kind.is_direct() && row.call == CallSupport::Native);
        if !direct {
            if row.as_ref().is_some_and(|row| row.kind.is_direct()) {
                return error(
                    ErrorCode::Unsupported,
                    "This conversation has no calls tinyclient can join.",
                );
            }
            return match self.tiny_join(conversation.account(), conversation.id) {
                Response::Ok => self.status(),
                refused => refused,
            };
        }
        if self.hub.link(&conversation.account()) != Link::Connected {
            self.hub.calls.show_failed(
                &self.hub,
                conversation,
                protocol::CallFailure::Lost,
                adapters::tiny::live::OFFLINE,
            );
            return self.status();
        }
        let attempt = match self.hub.calls.place(&self.hub, conversation.clone()) {
            Ok(attempt) => attempt,
            Err(err) => return call_error(err),
        };
        let direct_id = conversation.id.clone();
        match self.ask(&conversation.account(), |reply| {
            Command::Tiny(adapters::tiny::live::Ask::Ring { direct_id, attempt: Some(attempt), reply })
        }) {
            Ok(()) => self.status(),
            Err(refused) => {
                self.hub.calls.fail_ring_attempt(
                    &self.hub,
                    &conversation,
                    attempt,
                    protocol::CallFailure::Refused,
                    &response_detail(&refused),
                );
                *refused
            }
        }
    }

    /// Accepts the ring that is showing. A Tiny direct call does not start mediad here: the
    /// server says `call.accepted`, and the offer goes out then. Every other network starts
    /// mediad and joins at once.
    fn call_accept(&self) -> Response {
        let Some(conversation) = self.hub.calls.incoming(&self.hub) else {
            return call_error(CallError::NoCall);
        };
        if conversation.network == Network::Tiny {
            self.hub.calls.clicked(&conversation);
            self.command(&conversation.account(), Command::CallJoin(conversation.id));
            return self.status();
        }
        match self.hub.calls.accept(&self.hub) {
            Ok(conversation) => {
                self.join(conversation);
                self.status()
            }
            Err(err) => call_error(err),
        }
    }

    /// Has the adapter join, muted when the user joins muted (V4).
    fn join(&self, conversation: Key) {
        let account = conversation.account();
        self.command(&account, Command::CallJoin(conversation.id));
        if self.hub.settings().on(settings::JOIN_MUTED) {
            self.command(&account, Command::CallAudio { muted: true, deafened: false });
        }
    }

    pub fn handle(&self, request: Request) -> Response {
        // A seeded demo's Tiny server has no HTTP behind it: the demo answers for it.
        if let Some(answer) = crate::demo::tiny_answer(&self.hub, &request) {
            return answer;
        }
        match request {
            Request::Ping => Response::Pong,
            Request::Status => self.status(),
            Request::Wait { since } => {
                let (seq, reset, changes) = self.hub.wait(since, WAIT);
                Response::Changes { seq, reset, changes }
            }

            Request::Inbox { network, filter, offset, limit } => {
                let hidden = self.withheld();
                let read = {
                    let store = self.hub.store();
                    let page = store.inbox(network, filter, offset, limit.min(PAGE_LIMIT), &hidden);
                    page.and_then(|(conversations, total)| Ok((conversations, total, store.inbox_counts(network, &hidden)?)))
                };
                match read {
                    Ok((conversations, total, counts)) => Response::Conversations {
                        conversations: self.rows(conversations),
                        total,
                        counts,
                    },
                    Err(err) => internal(err),
                }
            }
            Request::Home { network, account, folder, shelf, offset, limit } => {
                let hidden = self.withheld();
                let read = self.hub.store().home(network, account.as_ref(), folder.as_deref(), shelf, offset, limit.min(PAGE_LIMIT), &hidden);
                if offset == 0 {
                    for linked in self.hub.links() {
                        let mine = linked.account.network == network && account.as_ref().is_none_or(|account| *account == linked.account);
                        if mine && linked.link == Link::Connected {
                            self.command(&linked.account, Command::OpenHome);
                        }
                    }
                }
                match read {
                    Ok((conversations, total, counts)) => Response::Conversations { conversations: self.rows(conversations), total, counts },
                    Err(err) => internal(err),
                }
            }
            Request::Folders { network } => match self.hub.store().folders(Some(network)) {
                Ok(folders) => Response::Folders { folders },
                Err(err) => internal(err),
            },
            Request::Threads { space, offset, limit } => {
                if !self.shows(&space.account()) {
                    // Same refusal as Tree. An empty Conversations page looks like a real empty list.
                    return hidden_list();
                }
                let read = self.hub.store().threads(&space, offset, limit.min(PAGE_LIMIT));
                if offset == 0 {
                    self.command(&space.account(), Command::OpenThreads(space.id.clone()));
                }
                match read {
                    Ok((conversations, total)) => {
                        Response::Conversations { conversations: self.rows(conversations), total, counts: InboxCounts::default() }
                    }
                    Err(err) => internal(err),
                }
            }
            Request::FollowThread { conversation, on } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.ask(&conversation.account(), |reply| Command::FollowThread {
                    conversation_id: conversation.id.clone(),
                    on,
                    reply,
                }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::Topics { forum, offset, limit, landing } => self.topics(forum, offset, limit, landing),
            Request::ForumInfo { forum } => {
                if !self.shows(&forum.account()) {
                    return error(ErrorCode::NotFound, "no such conversation");
                }
                match self.ask(&forum.account(), |reply| Command::ForumInfo {
                    conversation_id: forum.id.clone(),
                    reply,
                }) {
                    Ok(info) => Response::ForumInfo { info },
                    Err(refused) => *refused,
                }
            }
            Request::ForumPost { forum, title, text, tags, with } => {
                let account = forum.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                if !self.shows(&account) {
                    return error(ErrorCode::NotFound, "no such conversation");
                }
                match self.ask(&account, |reply| Command::ForumPost {
                    conversation_id: forum.id.clone(),
                    title,
                    text,
                    tags,
                    with: Box::new(with),
                    reply,
                }) {
                    Ok(thread_id) => {
                        let key = Key::new(&account, &thread_id);
                        match self.hub.store().conversation(&key) {
                            Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                            Ok(None) => error(ErrorCode::Internal, "post created, but not recorded"),
                            Err(err) => internal(err),
                        }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::InVoice { network } => self.in_voice(network),
            Request::Around { network } => self.around(network),
            Request::Catalog { account, kind, space, query, cursor } => self.catalog(account, kind, space, query, cursor),
            Request::SaveGif { account, id, unsave } => {
                match self.ask(&account, |reply| Command::SaveGif { id, unsave, reply }) {
                    Ok(()) => {
                        self.hub.catalogs.forget_where(|(acct, kind, ..)| acct == &account && *kind == CatalogKind::Gifs);
                        Response::Ok
                    }
                    Err(refused) => *refused,
                }
            }
            Request::ReadSessions => {
                let denied = self.hub.keychain().denied;
                if let Some(chat) = self.this.get().and_then(Weak::upgrade) {
                    if !denied.is_empty() {
                        Chat::read_sessions(&chat, denied, false);
                    }
                }
                self.status()
            }
            Request::Find { network, query, limit } => {
                let query = query.trim();
                if test_panic(query) {
                    let _held = self.hub.store();
                    panic!("a test asked this request to panic");
                }
                let found = match query.is_empty() {
                    true => Ok(Vec::new()),
                    false => self.hub.store().find(network, query, limit.min(PAGE_LIMIT)),
                };
                match found {
                    Ok(conversations) => {
                        let conversations: Vec<_> = conversations.into_iter().filter(|row| self.shows(&row.key.account())).collect();
                        Response::Conversations {
                            total: conversations.len() as u32,
                            conversations: self.rows(conversations),
                            counts: InboxCounts::default(),
                        }
                    }
                    Err(err) => internal(err),
                }
            }
            Request::SearchMessages { conversation, network, space, query, limit } => {
                let query = query.trim();
                let found = match query.is_empty() {
                    true => Ok(Vec::new()),
                    false => self.hub.store().search_messages(conversation.as_ref(), network, space.as_ref(), query, limit.min(PAGE_LIMIT)),
                };
                match found {
                    Ok(hits) => {
                        let hits: Vec<_> = hits.into_iter().filter(|hit| self.shows(&hit.conversation.account())).collect();
                        Response::MessageHits { hits, cursor: None, not_searched: Vec::new(), total: None }
                    }
                    Err(err) => internal(err),
                }
            }
            Request::SearchNetwork { conversation, space, account, network, query, filters, cursor } => {
                let query = query.trim().to_owned();
                if query.is_empty() && filters.is_empty() {
                    Response::MessageHits { hits: Vec::new(), cursor: None, not_searched: Vec::new(), total: None }
                } else if conversation.is_some() || space.is_some() {
                    let (target_account, scope) = match (&conversation, &space) {
                        (Some(key), _) => (key.account(), SearchScope::Conversation(key.id.clone())),
                        (None, Some(key)) => (key.account(), SearchScope::Space(key.id.clone())),
                        (None, None) => unreachable!(),
                    };
                    if !self.shows(&target_account) {
                        return Response::MessageHits { hits: Vec::new(), cursor: None, not_searched: Vec::new(), total: None };
                    }
                    match self.ask(&target_account, |reply| Command::Search {
                        query,
                        scope,
                        filters,
                        cursor,
                        at: std::time::Instant::now(),
                        reply,
                    }) {
                        Ok(page) => Response::MessageHits {
                            hits: page.hits,
                            cursor: page.cursor,
                            not_searched: Vec::new(),
                            total: page.total,
                        },
                        Err(refused) => *refused,
                    }
                } else {
                    // Everywhere, or scoped to one account or one network: fan out once to each
                    // connected account whose adapter supports account-wide search, merge by time,
                    // and honestly list connected networks that were not asked.
                    const PAGE: usize = 25;
                    let offset: usize = cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
                    let accounts: Vec<AccountKey> = match self.hub.store().accounts() {
                        Ok(rows) => rows
                            .into_iter()
                            .map(|row| row.key)
                            .filter(|k| self.shows(k))
                            .filter(|k| account.as_ref().is_none_or(|a| a == k))
                            .filter(|k| network.is_none_or(|n| n == k.network))
                            .collect(),
                        Err(err) => return internal(err),
                    };
                    // Single-account scope (e.g. Discord all-DMs): preserve the adapter's cursor and total.
                    if accounts.len() == 1 {
                        let acct = &accounts[0];
                        let can = self.hub.capabilities(acct);
                        if self.hub.link(acct) == Link::Connected && can.search_global {
                            return match self.ask(acct, |reply| Command::Search {
                                query,
                                scope: SearchScope::Account,
                                filters,
                                cursor,
                                at: std::time::Instant::now(),
                                reply,
                            }) {
                                Ok(page) => Response::MessageHits {
                                    hits: page.hits,
                                    cursor: page.cursor,
                                    not_searched: Vec::new(),
                                    total: page.total,
                                },
                                Err(refused) => *refused,
                            };
                        }
                        return Response::MessageHits {
                            hits: Vec::new(),
                            cursor: None,
                            not_searched: vec![acct.network],
                            total: None,
                        };
                    }
                    let mut merged: Vec<protocol::MessageHit> = Vec::new();
                    let mut not_searched: Vec<Network> = Vec::new();
                    let mut asked_networks: Vec<Network> = Vec::new();
                    for acct in accounts {
                        let can = self.hub.capabilities(&acct);
                        if self.hub.link(&acct) == Link::Connected && can.search_global {
                            if let Ok(page) = self.ask(&acct, |reply| Command::Search {
                                query: query.clone(),
                                scope: SearchScope::Account,
                                filters: filters.clone(),
                                cursor: None,
                                at: std::time::Instant::now(),
                                reply,
                            }) {
                                if !asked_networks.contains(&acct.network) {
                                    asked_networks.push(acct.network);
                                }
                                merged.extend(page.hits);
                                continue;
                            }
                        }
                        if !not_searched.contains(&acct.network) {
                            not_searched.push(acct.network);
                        }
                    }
                    not_searched.retain(|n| !asked_networks.contains(n));
                    merged.sort_by(|a, b| b.sent.cmp(&a.sent).then_with(|| b.message_id.cmp(&a.message_id)));
                    let mut seen = std::collections::HashSet::new();
                    merged.retain(|hit| seen.insert((hit.conversation.clone(), hit.message_id.clone())));
                    let next_cursor = (offset + PAGE < merged.len()).then(|| (offset + PAGE).to_string());
                    let page: Vec<protocol::MessageHit> = merged.into_iter().skip(offset).take(PAGE).collect();
                    Response::MessageHits { hits: page, cursor: next_cursor, not_searched, total: None }
                }
            }
            Request::SetDraft { conversation, text } => {
                let answer = match self.hub.store().set_draft(&conversation, &text) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(error(ErrorCode::NotFound, "no such conversation")),
                    Err(err) => Err(internal(err)),
                };
                // The account hears of the settled draft beside the store, so what the
                // networks that keep one are told of it.
                if answer.is_ok() && self.hub.link(&conversation.account()) == Link::Connected {
                    self.command(&conversation.account(), Command::Draft { conversation_id: conversation.id.clone(), text });
                }
                match answer {
                    Ok(()) => Response::Ok,
                    Err(response) => response,
                }
            }
            Request::Spaces { network } => match self.hub.store().spaces(network) {
                Ok(spaces) => {
                    let spaces = spaces.into_iter().filter(|space| self.shows(&space.key.account())).collect();
                    Response::Spaces { spaces }
                }
                Err(err) => internal(err),
            },
            Request::Tree { space, offset, limit, peek } => {
                if !self.shows(&space.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                let tree = self.hub.store().tree(&space, offset, limit.min(TREE_LIMIT));
                match tree {
                    Ok(Some(tree)) => {
                        if offset == 0 && !peek {
                            self.command(&space.account(), Command::OpenSpace(space.id.clone()));
                        }
                        Response::Tree {
                            space,
                            groups: tree.groups,
                            conversations: self.rows(tree.conversations),
                            total: tree.total,
                        }
                    }
                    Ok(None) => error(ErrorCode::NotFound, "no such space"),
                    Err(err) => internal(err),
                }
            }
            Request::Conversation { conversation } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.hub.store().conversation(&conversation) {
                    Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                    Ok(None) => error(ErrorCode::NotFound, "no such conversation"),
                    Err(err) => internal(err),
                }
            }
            Request::Collapse { space, group, collapsed } => {
                match self.hub.store().set_collapsed(&space, &group, collapsed) {
                    Ok(true) => Response::Ok,
                    Ok(false) => error(ErrorCode::NotFound, "no such group"),
                    Err(err) => internal(err),
                }
            }
            Request::Star { conversation, starred } => {
                // On Slack the star is Slack's. Other networks keep the local flag and are not asked.
                if conversation.network == Network::Slack {
                    if let Err(refused) = self.online(&conversation.account()) {
                        return *refused;
                    }
                    let conversation_id = conversation.id.clone();
                    return match self.ask(&conversation.account(), |reply| Command::Star { conversation_id, starred, reply }) {
                        Ok(()) => Response::Ok,
                        Err(refused) => *refused,
                    };
                }
                match self.hub.store().set_starred(&conversation, starred) {
                    Ok(changed) => {
                        if changed {
                            self.hub.changed(Change::Conversation { conversation });
                        }
                        Response::Ok
                    }
                    Err(err) => internal(err),
                }
            }
            Request::File { conversation, pinned, archived, folder_add, folder_remove } => {
                if pinned.is_none() && archived.is_none() && folder_add.is_none() && folder_remove.is_none() {
                    return Response::Ok;
                }
                self.act(&conversation, |conversation_id, reply| Command::File {
                    conversation_id,
                    pinned,
                    archived,
                    folder_add,
                    folder_remove,
                    reply,
                })
            }
            Request::Remind { conversation, message_id, remind_at } => {
                let account = conversation.account();
                if !self.shows(&account) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                if remind_at.is_some_and(|due| due <= now()) {
                    return error(ErrorCode::Refused, "That time has passed.");
                }
                if self.online(&account).is_ok() {
                    if let Err(refused) = self.ask(&account, |reply| Command::Remind {
                        conversation_id: conversation.id.clone(),
                        message_id: message_id.clone(),
                        remind_at,
                        reply,
                    }) {
                        if !matches!(*refused, Response::Error { code: ErrorCode::Unsupported, .. }) {
                            return *refused;
                        }
                    }
                }
                match self.hub.store().remind(&conversation, &message_id, remind_at) {
                    Ok(crate::store::RemindOutcome::Set | crate::store::RemindOutcome::Cleared) => {
                        // The notification thread's next wait is due at a different time now.
                        self.hub.notify(crate::notify::Event::Remind);
                        Response::Ok
                    }
                    Ok(crate::store::RemindOutcome::Missing) => error(ErrorCode::NotFound, "no such message"),
                    Ok(crate::store::RemindOutcome::Full) => {
                        error(ErrorCode::Refused, format!("You have {} reminders already. Cancel one first.", crate::store::REMINDER_CAP))
                    }
                    Err(err) => internal(err),
                }
            }
            Request::Reminders { conversation } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.hub.store().reminders(&conversation) {
                    Ok(reminders) => Response::Reminders { conversation, reminders },
                    Err(err) => internal(err),
                }
            }
            Request::ScheduledMessages { conversation } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.ask(&conversation.account(), |reply| Command::ScheduledList {
                    conversation_id: conversation.id.clone(),
                    reply,
                }) {
                    Ok(messages) => Response::ScheduledMessages { messages },
                    Err(refused) => *refused,
                }
            }
            Request::CancelScheduled { conversation, scheduled_id } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.ask(&conversation.account(), |reply| Command::CancelScheduled {
                    conversation_id: conversation.id.clone(),
                    scheduled_id,
                    reply,
                }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::RescheduleMessage { conversation, scheduled_id, text, post_at } => {
                if !self.shows(&conversation.account()) {
                    return error(ErrorCode::NotFound, "signed out");
                }
                match self.ask(&conversation.account(), |reply| Command::Reschedule {
                    conversation_id: conversation.id.clone(),
                    scheduled_id,
                    text,
                    post_at,
                    reply,
                }) {
                    Ok(message) => Response::ScheduledMessage { message },
                    Err(refused) => *refused,
                }
            }

            Request::People { network, relation, query, offset, limit } => {
                self.people(network, relation, query, offset, limit)
            }
            Request::Members { conversation, offset, limit } => {
                let conversation_id = conversation.id.clone();
                let limit = limit.min(100);
                let asked = self.ask(&conversation.account(), |reply| Command::Members {
                    conversation_id,
                    offset,
                    limit,
                    reply,
                });
                match asked {
                    Ok(found) => Response::Members {
                        pending: found.pending,
                        conversation,
                        sections: found.sections,
                        offset,
                        members: found.members,
                    },
                    Err(refused) => *refused,
                }
            }
            Request::MembersAdd { conversation, people } => {
                if people.is_empty() {
                    return error(ErrorCode::Refused, "Pick at least one person to add.");
                }
                let res = self.act(&conversation, |conversation_id, reply| Command::MembersAdd { conversation_id, people, reply });
                if res == Response::Ok {
                    self.hub.infos.forget(&conversation);
                }
                res
            }
            Request::MembersRemove { conversation, person } => {
                if person.trim().is_empty() {
                    return error(ErrorCode::Refused, "No person named to remove.");
                }
                let res = self.act(&conversation, |conversation_id, reply| Command::MembersRemove { conversation_id, person, reply });
                if res == Response::Ok {
                    self.hub.infos.forget(&conversation);
                }
                res
            }
            Request::ConversationUpdate { conversation, title, topic, description } => {
                if title.is_none() && topic.is_none() && description.is_none() {
                    return Response::Ok;
                }
                if title.as_deref().is_some_and(|t| t.trim().is_empty()) {
                    return error(ErrorCode::Refused, "A conversation name cannot be empty.");
                }
                let res = self.act(&conversation, |conversation_id, reply| Command::ConversationUpdate {
                    conversation_id,
                    title,
                    topic,
                    description,
                    reply,
                });
                if res == Response::Ok {
                    self.hub.infos.forget(&conversation);
                }
                res
            }
            Request::ConversationPicture { conversation, path } => {
                let res = self.act(&conversation, |conversation_id, reply| Command::ConversationPicture { conversation_id, path, reply });
                if res == Response::Ok {
                    self.hub.infos.forget(&conversation);
                }
                res
            }
            request @ (Request::StartDirect { .. } | Request::StartDirectCall { .. }) => {
                let (person, call) = match request {
                    Request::StartDirect { person } => (person, false),
                    Request::StartDirectCall { person } => (person, true),
                    _ => unreachable!(),
                };
                let person_id = person.id.clone();
                let started = self.ask(&person.account(), |reply| {
                    if call && person.network == Network::Tiny {
                        Command::StartDirectCall { person_id, reply }
                    } else {
                        Command::StartDirect { person_id, reply }
                    }
                });
                let id = match started {
                    Ok(id) => id,
                    Err(refused) => return *refused,
                };
                match self.hub.store().conversation(&Key::new(&person.account(), id)) {
                    Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                    Ok(None) => error(ErrorCode::Internal, "started, but not recorded"),
                    Err(err) => internal(err),
                }
            }
            Request::StartGroup { account, people, title } => {
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let cap = self.hub.capabilities(&account).groups;
                if cap == 0 {
                    return error(ErrorCode::Unsupported, "Creating group chats is not supported on this account.");
                }
                let mut ids: Vec<String> = Vec::new();
                for p in people {
                    if p.account() != account {
                        return error(ErrorCode::Refused, "Everyone in a group must be on the same account.");
                    }
                    if !ids.contains(&p.id) {
                        ids.push(p.id);
                    }
                }
                if ids.is_empty() {
                    return error(ErrorCode::Refused, "Pick at least one person for the group.");
                }
                if ids.len() > cap as usize {
                    return error(ErrorCode::Refused, format!("This account allows at most {cap} people in a group."));
                }
                let title = title.map(|mut t| {
                    protocol::clip(&mut t, 100);
                    t.trim().to_owned()
                }).filter(|t| !t.is_empty());
                let id = match self.ask(&account, |reply| Command::StartGroup { people: ids, title, reply }) {
                    Ok(id) => id,
                    Err(refused) => return *refused,
                };
                match self.hub.store().conversation(&Key::new(&account, id)) {
                    Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                    Ok(None) => error(ErrorCode::Internal, "group created, but not recorded"),
                    Err(err) => internal(err),
                }
            }
            Request::CreateChannel { space, name, kind, private, group } => {
                let account = space.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let mut name = name.trim().to_owned();
                protocol::clip(&mut name, 100);
                if name.is_empty() {
                    return error(ErrorCode::Refused, "Channel name cannot be empty.");
                }
                let space_id = space.id.clone();
                let id = match self.ask(&account, |reply| Command::CreateChannel { space_id, name, kind, private, group, reply }) {
                    Ok(id) => id,
                    Err(refused) => return *refused,
                };
                match self.hub.store().conversation(&Key::new(&account, id)) {
                    Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                    Ok(None) => error(ErrorCode::Internal, "channel created, but not recorded"),
                    Err(err) => internal(err),
                }
            }
            Request::CreateGroup { account, members, name } => {
                let started = self.ask(&account, |reply| Command::CreateGroup {
                    members,
                    name,
                    reply,
                });
                let id = match started {
                    Ok(id) => id,
                    Err(refused) => return *refused,
                };
                match self.hub.store().conversation(&Key::new(&account, id)) {
                    Ok(Some(conversation)) => Response::Conversation { conversation: self.row(conversation) },
                    Ok(None) => error(ErrorCode::Internal, "group created, but not recorded"),
                    Err(err) => internal(err),
                }
            }
            Request::StartThread { conversation, message } => self.start_thread(conversation, message),
            Request::Profile { person, space } => self.profile(person, space),
            Request::ConversationInfo { conversation } => self.conversation_info(conversation),
            Request::SpaceInfo { space } => self.space_info(space),
            Request::SetMute { conversation, notify, until } => {
                self.act(&conversation, |conversation_id, reply| Command::SetMute { conversation_id, notify, until, reply })
            }
            Request::SetDisappearing { conversation, seconds } => {
                let res = self.act(&conversation, |conversation_id, reply| Command::SetDisappearing { conversation_id, seconds, reply });
                if res == Response::Ok {
                    self.hub.infos.forget(&conversation);
                }
                res
            }
            Request::Leave { conversation } => self.act(&conversation, |conversation_id, reply| Command::Leave { conversation_id, reply }),
            Request::LeaveSpace { space } => self.leave_space(space),
            Request::CloseDirect { conversation } => {
                self.act(&conversation, |conversation_id, reply| Command::CloseDirect { conversation_id, reply })
            }
            Request::ClearHistory { conversation, delete_chat, revoke } => {
                self.act(&conversation, |conversation_id, reply| Command::ClearHistory { conversation_id, delete_chat, revoke, reply })
            }
            Request::AnswerMessageRequest { conversation, accept } => {
                self.act(&conversation, |conversation_id, reply| Command::AnswerMessageRequest { conversation_id, accept, reply })
            }
            Request::Pinned { conversation } => self.pinned(conversation),
            Request::Pin { conversation, message_id, pinned, notify, me_only } => {
                self.act(&conversation, |conversation_id, reply| Command::Pin {
                    conversation_id,
                    message_id,
                    pinned,
                    notify,
                    me_only,
                    reply,
                })
            }
            Request::StarMessage { conversation, message_id, starred } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                match self.ask(&account, |reply| Command::StarMessage {
                    conversation_id: conversation.id.clone(),
                    message_id,
                    starred,
                    reply,
                }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::StarredMessages { account, conversation, limit } => {
                let conv_id = conversation.as_ref().map(|c| c.id.clone());
                let key = conversation.unwrap_or_else(|| Key::new(&account, ""));
                if self.online(&account).is_ok() {
                    match self.ask(&account, |reply| Command::StarredMessages {
                        conversation_id: conv_id.clone(),
                        limit,
                        reply,
                    }) {
                        Ok(mut messages) => {
                            messages.iter_mut().for_each(Message::bound);
                            return Response::Pinned { conversation: key, messages };
                        }
                        Err(refused) if !matches!(*refused, Response::Error { code: ErrorCode::Unsupported, .. }) => {
                            return *refused;
                        }
                        Err(_) => {}
                    }
                }
                match self.hub.store().starred_messages(&account, conv_id.as_deref(), limit) {
                    Ok(mut messages) => {
                        messages.iter_mut().for_each(Message::bound);
                        Response::Pinned { conversation: key, messages }
                    }
                    Err(err) => internal(err),
                }
            }
            Request::Shared { conversation, kind, before, limit, search } => self.shared(conversation, kind, before, limit, search),
            Request::CallHistory { account, missed, before, limit } => self.call_history(account, missed, before, limit),
            Request::DeleteCallEntries { account, message_ids, revoke } => self.delete_call_entries(account, message_ids, revoke),
            Request::GetPrivacy { account, key } => self.get_privacy(account, key),
            Request::SetPrivacy { account, key, policy } => self.set_privacy(account, key, policy),
            Request::Relate { person, action } => self.relate(person, action),
            Request::AddFriend { account, username } => self.add_friend(account, username),
            Request::Report { target, reason, note, block } => self.report(target, reason, note, block),
            Request::ReportReasons { target } => self.report_reasons(target),
            Request::SetPresence { account, presence, status, status_emoji, clear_at } => self.set_presence(account, presence, status, status_emoji, clear_at),
            Request::SetProfile { account, name, about, pronouns, emoji, username, photo } => {
                self.set_profile(account, name, about, pronouns, emoji, username, photo)
            }
            Request::SetAvatar { account, path } => self.set_avatar(account, path),
            Request::SetNickname { space, nickname } => self.set_nickname(space, nickname),
            Request::SetFriendNickname { person, nickname } => self.set_friend_nickname(person, nickname),
            Request::InvitePreview { account, code, event_id } => self.invite_preview(account, code, event_id),
            Request::InviteJoin { account, code } => self.invite_join(account, code),
            Request::EventPreview { space, event_id } => self.event_preview(space, event_id),
            Request::EventInterest { space, event_id, interested } => self.event_interest(space, event_id, interested),
            Request::Watch(watching) => self.watch(watching),

            Request::Messages { conversation, page, limit } => self.messages(conversation, page, limit),
            Request::Revisions { conversation, message_id } => self.revisions(conversation, message_id),
            Request::Source { conversation, message_id } => self.source(conversation, message_id),
            Request::Resolve { url } => self.resolve_link(&url),
            Request::MarkRead { conversation, message_id } => self.mark_read(conversation, message_id),
            Request::MarkUnread { conversation, message_id } => self.mark_unread(conversation, message_id),
            Request::MarkAllRead { network, space } => self.mark_all_read(network, space),
            Request::Forget { conversation } => self.forget_conversation(conversation),
            Request::Send { conversation, client_id, text, reply_to, with } => {
                let began = std::time::Instant::now();
                if conversation.network == Network::Tiny {
                    crate::sendtime::submitted(&client_id);
                }
                let logged = client_id.clone();
                let message = self.new_message(&conversation, client_id, text, reply_to.as_deref(), &with);
                let pending = began.elapsed();
                let answer = self.send(conversation, message, reply_to, with);
                let (took, row) = (began.elapsed().as_millis(), pending.as_millis());
                tracing::info!(client_id = %logged, elapsed_ms = took as u64, pending_row_ms = row as u64, "chatd: send submitted");
                answer
            }
            Request::CancelSend { conversation, client_id } => {
                let message = match self.unsent(&conversation, &client_id) {
                    Ok(message) => message,
                    Err(refused) => return *refused,
                };
                if message.waiting {
                    let conversation_id = conversation.id.clone();
                    self.command(&conversation.account(), Command::CancelSend { conversation_id, client_id: client_id.clone() });
                    return self.drop_unsent(conversation, &client_id, &message);
                }
                let conversation_id = conversation.id.clone();
                self.command(&conversation.account(), Command::CancelSend { conversation_id, client_id });
                Response::Ok
            }
            Request::Retry { conversation, client_id } => match self.unsent(&conversation, &client_id) {
                Ok(message) => {
                    let reply_to = message.reply_to.as_ref().map(|quoted| quoted.id.clone());
                    // The files go again from where they were; the options were the first try's.
                    let with = Outgoing { files: outgoing_files(&message), ..Outgoing::default() };
                    self.send(conversation, message, reply_to, with)
                }
                Err(refused) => *refused,
            },
            Request::Discard { conversation, client_id } => {
                let message = match self.unsent(&conversation, &client_id) {
                    Ok(message) => message,
                    Err(refused) => return *refused,
                };
                if message.waiting {
                    let conversation_id = conversation.id.clone();
                    self.command(&conversation.account(), Command::CancelSend { conversation_id, client_id: client_id.clone() });
                }
                self.drop_unsent(conversation, &client_id, &message)
            }
            Request::Typing { conversation, activity } => {
                let account = conversation.account();
                // P1: off, the network never hears it.
                if self.hub.link(&account) == Link::Connected && self.hub.settings().typing(&account) {
                    self.command(&account, Command::Typing { conversation_id: conversation.id, activity });
                }
                Response::Ok
            }
            Request::FollowLink { conversation, message_id } => {
                let account = conversation.account();
                if self.hub.link(&account) == Link::Connected {
                    self.command(&account, Command::FollowLink { conversation_id: conversation.id, message_id });
                }
                Response::Ok
            }

            Request::React { conversation, message_id, emoji, on, burst } => self.react(conversation, message_id, emoji, on, burst),
            Request::Reactions { conversation } => self.reactions(conversation),
            Request::Reactors { conversation, message_id, emoji, burst, after, limit } => {
                self.reactors(conversation, message_id, emoji, burst, after, limit)
            }
            Request::Edit { conversation, message_id, text, spans } => self.rewrite(conversation, message_id, Some(text), spans, protocol::DeleteScope::Everyone),
            Request::Delete { conversation, message_id, scope } => self.rewrite(conversation, message_id, None, Vec::new(), scope),
            Request::Forward { from, message_ids, to, note } => self.forward(from, message_ids, to, note),

            Request::Poll { conversation, message_id, votes } => self.poll(conversation, message_id, votes),

            Request::Fetch { account, ids, size, retry } => {
                let adapter = lock(&self.adapters).get(&account).map(Adapter::sender);
                let ids = &ids[..ids.len().min(PAGE_LIMIT as usize)];
                // What the cache is asked for is decided here: a failure the user wants tried
                // again is forgotten, and a picture wanted larger than the one held is pointed
                // at a larger registered size, which makes it not fetched yet.
                let prepared = {
                    let mut store = self.hub.store();
                    let retried = if retry { store.media_retry(&account, ids).map(drop) } else { Ok(()) };
                    retried.and_then(|()| match size {
                        Some(size) => store.media_sized(&account, ids, size.min(LARGEST)).map(drop),
                        None => Ok(()),
                    })
                };
                if let Err(err) = prepared {
                    return internal(err);
                }
                if retry {
                    self.blobs.retried(&account, ids);
                }
                match self.blobs.request(&account, ids, adapter.clone()) {
                    Ok(files) => {
                        // What the cache has never heard of may be the adapter's to fetch itself.
                        let unknown: Vec<String> = ids.iter().filter(|id| !files.iter().any(|file| file.id == **id)).cloned().collect();
                        if let (false, Some(adapter)) = (unknown.is_empty(), adapter) {
                            let _ = adapter.send(Command::Wanted(unknown));
                        }
                        Response::Media { files }
                    }
                    Err(err) => internal(err),
                }
            }

            Request::SignIn { network, at } => self.sign_in(network, at),
            Request::SignInBegin { network } => self.sign_in_step(network, None),
            Request::SignInAnswer { network, input } => self.sign_in_step(network, Some(input)),
            Request::SignInCancel { network } => {
                match network {
                    Network::Telegram => adapters::telegram::signin::cancel(),
                    Network::Signal => adapters::signal::signin::cancel(),
                    Network::Messages => adapters::messages::signin::cancel(),
                    Network::Tiny => adapters::tiny::signin::cancel(),
                    _ => {}
                }
                Response::Ok
            }
            Request::SignOut { account, erase } => self.sign_out(account, erase),
            Request::TinyLocalServers => adapters::tiny::local_servers(),
            Request::TinyCommunity { account } => {
                // Opening the server is the user asking: a socket another device replaced connects again.
                self.command(&account, Command::Tiny(adapters::tiny::live::Ask::Look));
                adapters::tiny::community(&self.hub, account)
            }
            Request::TinyRoomCreate { account, name } => adapters::tiny::create_room(account, name),
            Request::TinyRoomRename { account, room_id, name } => adapters::tiny::rename_room(account, room_id, name),
            Request::TinyRoomDelete { account, room_id } => adapters::tiny::delete_room(account, room_id),
            Request::TinyCallJoin { account, room_id } => self.tiny_join(account, room_id),
            Request::TinyCallLeave { account } => {
                // Leaving is the person's own to do: the call ends here first, and the shell
                // hears at once, before the server is asked or when it cannot be.
                let left = self.hub.calls.end_in(&self.hub, &account);
                match self.tiny_call(&account, "community.calls.join", &[], |reply| adapters::tiny::live::Ask::Leave { reply }) {
                    // Hung up here. A server that was not told lets the seat go itself, or is
                    // told when its socket is back (`seat_kept`).
                    Response::Error { .. } if left => Response::Ok,
                    answer => answer,
                }
            }
            Request::TinyCallKick { account, room_id, user_id } => {
                let ids = [room_id.clone(), user_id.clone()];
                self.tiny_call(&account, "community.calls.kick", &ids, |reply| adapters::tiny::live::Ask::Kick { room_id, user_id, reply })
            }
            Request::TinyVerify { account, user_id, key, verified } => match crate::verified::set(&account, &user_id, &key, verified) {
                Ok(()) => {
                    self.hub.calls.reverify(&self.hub, &account);
                    Response::Ok
                }
                Err(err) => error(ErrorCode::Refused, format!("That key couldn't be remembered: {err}")),
            },
            Request::TinyE2ee { account, ask } => {
                let (hub, changed) = (self.hub.clone(), account.clone());
                adapters::tiny::e2ee_ask(account, ask, Arc::new(move || hub.changed(Change::Tiny { account: changed.clone() })))
            }
            Request::TinyInvite { account, role } => adapters::tiny::invite(account, role),
            Request::TinyRole { account, user_id, role } => adapters::tiny::set_role(account, user_id, role),
            Request::TinyRemove { account, user_id } => adapters::tiny::remove(account, user_id),
            Request::TinyRevoke { account, invite_id } => adapters::tiny::revoke(account, invite_id),
            Request::TinyRename { account, display_name } => adapters::tiny::rename(account, display_name),
            // Ended on the server first, then forgotten here exactly as one sign-out is.
            Request::TinySignOutEverywhere { account } => match adapters::tiny::end_sessions(&account) {
                Ok(()) => self.sign_out(account, false),
                Err(answer) => answer,
            },
            Request::TinyDeleteAccount { account } => match adapters::tiny::manage::delete_account(&account) {
                Ok(()) => self.forget_deleted_tiny(account),
                Err(answer) => answer,
            },
            Request::TinyReport { account, kind, target_id, category, note, evidence } => {
                adapters::tiny::manage::report_create(account, kind, target_id, category, note, evidence)
            }
            Request::TinyEmojiList { account } => adapters::tiny::manage::emoji_list(account),
            Request::TinyEmojiCreate { account, name, kind, path } => adapters::tiny::manage::emoji_create(account, name, kind, path),
            Request::TinyEmojiRename { account, emoji_id, name } => adapters::tiny::manage::emoji_rename(account, emoji_id, name),
            Request::TinyEmojiDelete { account, emoji_id } => adapters::tiny::manage::emoji_delete(account, emoji_id),
            Request::TinyServerUpdate { account, edit } => adapters::tiny::manage::server_update(account, edit),
            Request::TinyServerIcon { account, path } => adapters::tiny::manage::server_icon(account, path),
            Request::TinyAvatar { account, path } => adapters::tiny::manage::user_avatar(account, path),
            Request::TinyRoleCreate { account, role } => adapters::tiny::manage::role_create(account, role),
            Request::TinyRoleUpdate { account, role_id, role } => adapters::tiny::manage::role_update(account, role_id, role),
            Request::TinyRoleDelete { account, role_id } => adapters::tiny::manage::role_delete(account, role_id),
            Request::TinyRoleOrder { account, role_ids } => adapters::tiny::manage::role_order(account, role_ids),
            Request::TinyMemberRoles { account, user_id, role_ids } => adapters::tiny::manage::member_roles(account, user_id, role_ids),
            Request::TinyMemberOwner { account, user_id, owner } => adapters::tiny::manage::member_owner(account, user_id, owner),
            Request::TinyTimeout { account, user_id, until, reason } => adapters::tiny::manage::timeout(account, user_id, until, reason),
            Request::TinyKick { account, user_id, reason } => adapters::tiny::manage::kick(account, user_id, reason),
            Request::TinyBan { account, user_id, reason, expires_at } => adapters::tiny::manage::ban(account, user_id, reason, expires_at),
            Request::TinyUnban { account, user_id } => adapters::tiny::manage::unban(account, user_id),
            Request::TinyCategoryCreate { account, name } => adapters::tiny::manage::category_create(account, name),
            Request::TinyCategoryRename { account, category_id, name } => adapters::tiny::manage::category_rename(account, category_id, name),
            Request::TinyCategoryDelete { account, category_id } => adapters::tiny::manage::category_delete(account, category_id),
            Request::TinyLayout { account, groups } => adapters::tiny::manage::layout(account, groups),
            Request::TinyRoomCreateWith { account, room } => adapters::tiny::manage::room_create(account, room),
            Request::TinyChannelCreate { account, channel } => adapters::tiny::manage::channel_create(account, channel),
            Request::TinyChannelUpdate { account, channel_id, channel } => adapters::tiny::manage::channel_update(account, channel_id, channel),
            Request::TinyChannelDelete { account, channel_id } => adapters::tiny::manage::channel_delete(account, channel_id),
            Request::TinyRoomUpdate { account, room_id, room } => adapters::tiny::manage::room_update(account, room_id, room),
            Request::TinyOverrideSet { account, room_id, role_id, allow, deny } => adapters::tiny::manage::override_set(account, room_id, role_id, allow, deny),
            Request::TinyOverrideClear { account, room_id, role_id } => adapters::tiny::manage::override_clear(account, room_id, role_id),
            Request::TinyChannelOverrideSet { account, channel_id, role_id, allow, deny } => adapters::tiny::manage::channel_override_set(account, channel_id, role_id, allow, deny),
            Request::TinyChannelOverrideClear { account, channel_id, role_id } => adapters::tiny::manage::channel_override_clear(account, channel_id, role_id),
            Request::TinyInviteWith { account, options } => adapters::tiny::manage::invite_with(account, options),
            Request::TinyAudit { account, filter } => adapters::tiny::manage::audit(account, filter),
            Request::TinyReportsList { account, filter } => adapters::tiny::manage::reports_list(account, filter),
            Request::TinyReportRead { account, report_id } => adapters::tiny::manage::report_read(account, report_id),
            Request::TinyReportWork { account, report_id, update } => adapters::tiny::manage::report_work(account, report_id, update),
            Request::TinySetupRead { account } => adapters::tiny::manage::setup_read(account),
            Request::TinySetupApply { account, apply } => adapters::tiny::manage::setup_apply(account, apply),
            Request::TinyPasskeyLink { account } => adapters::tiny::manage::passkey_link(account),
            Request::TinySetupClaimLinks { account, user_ids } => adapters::tiny::manage::setup_claim_links(account, user_ids),
            Request::TinyPasskeysRead { account } => adapters::tiny::manage::passkeys_read(account),
            Request::TinyPasskeyRename { account, id, label } => adapters::tiny::manage::passkey_rename(account, id, label),
            Request::TinyPasskeyRemove { account, id } => adapters::tiny::manage::passkey_remove(account, id),
            Request::TinyRecoveryCodes { account } => adapters::tiny::manage::recovery_codes(account),
            Request::TinyBots { account } => adapters::tiny::manage::bots_list(account),
            Request::TinyBotCreate { account, name, scopes } => adapters::tiny::manage::bots_create(account, name, scopes),
            Request::TinyBotRevoke { account, bot_id } => adapters::tiny::manage::bots_revoke(account, bot_id),
            Request::TinyCallModerate { account, room_id, user_id, server_muted, server_deafened } => {
                let mut needs = Vec::new();
                if server_muted.is_some() {
                    needs.push("community.calls.mute");
                }
                if server_deafened.is_some() {
                    needs.push("community.calls.deafen");
                }
                if needs.is_empty() {
                    return Response::Ok;
                }
                let ids = [room_id.as_str(), user_id.as_str()];
                let ask = |reply| adapters::tiny::live::Ask::Moderate { room_id: room_id.clone(), user_id: user_id.clone(), server_muted, server_deafened, reply };
                self.tiny_moderate(&account, &needs, &ids, ask)
            }
            Request::TinyCallMove { account, user_id, room_id } => {
                let ids = [room_id.as_str(), user_id.as_str()];
                let ask = |reply| adapters::tiny::live::Ask::Move { user_id: user_id.clone(), room_id: room_id.clone(), reply };
                self.tiny_moderate(&account, &["community.calls.move"], &ids, ask)
            }
            // The key is the mediad's to act on; a priority key on a Tiny call is also the
            // server's to hear, as `call.state`.
            Request::CallTalk { held, priority } => match self.hub.calls.request(&self.hub, &MediaRequest::Talk { held, priority }) {
                Ok(_) => {
                    if let Some(call) = self.hub.calls.describe().0.filter(|call| call.conversation.network == Network::Tiny) {
                        let on = held && priority;
                        self.command(&call.conversation.account(), Command::Tiny(adapters::tiny::live::Ask::Priority { on }));
                    }
                    Response::Ok
                }
                Err(err) => call_error(err),
            },
            // Kept per account and person, and applied to the call at once if it is on that
            // account. A call that is not up is no reason to refuse: the choice is kept.
            Request::CallGain { account, user_id, percent, muted } => {
                let key = settings::gain_key(&user_id);
                let (scope, value) = (settings::account_scope(&account), settings::gain_value(percent, muted));
                let allowed = value.as_deref().map_or_else(|| settings::known(&scope, &key), |value| settings::valid(&scope, &key, value));
                if !allowed {
                    return error(ErrorCode::Refused, "That volume can't be kept for them.");
                }
                if let Err(err) = self.hub.set_setting(&scope, &key, value) {
                    return internal(err);
                }
                self.hub.calls.gain(&self.hub, &account, user_id, percent, muted);
                Response::Ok
            }
            // Only ever from the person's own click: nothing else turns a blocked call to clear.
            Request::CallCarryOnUnencrypted => match self.hub.calls.request(&self.hub, &MediaRequest::TinyTransport) {
                Ok(protocol::MediaResponse::Done) => Response::Ok,
                Ok(protocol::MediaResponse::Refused { detail }) => error(ErrorCode::Refused, detail),
                Ok(_) => error(ErrorCode::Unsupported, "This call can't carry on without encryption."),
                Err(err) => call_error(err),
            },
            Request::CallWarm => {
                self.hub.calls.prewarm(&self.hub);
                Response::Ok
            }
            Request::CallStats => match self.hub.calls.request(&self.hub, &MediaRequest::Stats) {
                Ok(protocol::MediaResponse::Stats(stats)) => Response::CallStats { stats },
                Ok(protocol::MediaResponse::Refused { detail }) => error(ErrorCode::Unsupported, detail),
                Ok(_) => error(ErrorCode::Unsupported, "This call can't report its connection."),
                Err(err) => call_error(err),
            },

            Request::CallStart { conversation } => self.call_start(conversation),
            Request::CallAccept => self.call_accept(),
            // A ring waiting beside a call is declined on its own. Ending the call does not
            // take that ring into the slot, so the next call the user places is not that ring.
            Request::CallDecline => {
                if let Some(conversation) = self.hub.calls.decline_waiting(&self.hub) {
                    self.command(&conversation.account(), Command::CallLeave(conversation.id));
                } else if let Some(conversation) = self.hub.calls.end(&self.hub) {
                    self.command(&conversation.account(), Command::CallLeave(conversation.id));
                }
                self.status()
            }
            Request::CallEnd => {
                if let Some(conversation) = self.hub.calls.end(&self.hub) {
                    self.command(&conversation.account(), Command::CallLeave(conversation.id));
                }
                self.status()
            }
            Request::CallSet { muted, deafened } => match self.hub.calls.set_audio(&self.hub, muted, deafened) {
                Ok(conversation) => {
                    self.command(&conversation.account(), Command::CallAudio { muted, deafened });
                    self.status()
                }
                Err(err) => call_error(err),
            },
            Request::CallRing { people } => {
                let Some(call) = self.hub.calls.describe().0 else {
                    return error(ErrorCode::NoCall, "There is no active call.");
                };
                let conversation_id = call.conversation.id.clone();
                match self.ask(&call.conversation.account(), |reply| Command::CallRing {
                    conversation_id,
                    people,
                    reply,
                }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::CallStopRinging { people } => {
                let Some(call) = self.hub.calls.describe().0 else {
                    return error(ErrorCode::NoCall, "There is no active call.");
                };
                let conversation_id = call.conversation.id.clone();
                match self.ask(&call.conversation.account(), |reply| Command::CallStopRinging {
                    conversation_id,
                    people,
                    reply,
                }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::CallAddPeople { people } => {
                let Some(call) = self.hub.calls.describe().0 else {
                    return error(ErrorCode::NoCall, "There is no active call.");
                };
                let account = call.conversation.account();
                let conversation_id = call.conversation.id.clone();
                match self.ask(&account, |reply| Command::CallAddPeople {
                    conversation_id,
                    people,
                    reply,
                }) {
                    Ok(id) => match self.hub.store().conversation(&Key::new(&account, id)) {
                        Ok(Some(conversation)) => Response::Conversation { conversation },
                        Ok(None) => Response::Ok,
                        Err(err) => error(ErrorCode::Internal, err.to_string()),
                    },
                    Err(refused) => *refused,
                }
            }
            Request::Handoff { conversation } => {
                let conversation_id = conversation.id.clone();
                match self.ask(&conversation.account(), |reply| Command::Handoff { conversation_id, reply }) {
                    Ok(url) => Response::Link { url },
                    Err(refused) => *refused,
                }
            }
            Request::Play(request) => match self.hub.player.request(&self.hub, &support_dir().join("media"), request) {
                Ok(()) => Response::Ok,
                Err(detail) => error(ErrorCode::Media, detail),
            },
            // Answered `Ok` whatever became of it: the recording's own word is `Change::Recording`.
            Request::Record(request) => {
                self.hub.recorder.request(&self.hub, request);
                Response::Ok
            }
            Request::Media(request) => {
                let picture = match &request {
                    MediaRequest::Camera { on } => Some(Picture::Camera(*on)),
                    MediaRequest::ShareSend { on, .. } => Some(Picture::Share(*on)),
                    MediaRequest::ShareWatch { source } => Some(Picture::Watch(source.clone())),
                    _ => None,
                };
                let asked = match request {
                    // Joining is the adapter's to ask for, with what the network handed out.
                    MediaRequest::Join(_) | MediaRequest::Rejoin(_) => {
                        return error(ErrorCode::Unsupported, "a call is joined with call_start")
                    }
                    MediaRequest::Meter { on } => self.hub.calls.meter(&self.hub, on),
                    request => self.hub.calls.request(&self.hub, &request).map(drop),
                };
                // A refusal never tells Discord we are sending. Other networks ignore the command.
                if asked.is_ok() {
                    if let Some(picture) = picture {
                        if let (Some(call), true) = self.hub.calls.describe() {
                            if call.conversation.network == Network::Discord {
                                self.command(&call.conversation.account(), Command::CallPicture(picture));
                            }
                        }
                    }
                }
                match asked {
                    Ok(()) => self.status(),
                    Err(err) => call_error(err),
                }
            }

            Request::Settings => match self.hub.store().settings() {
                Ok(values) => Response::Settings { values },
                Err(err) => internal(err),
            },
            Request::Set { scope, key, value } => self.set(scope, key, value),
            Request::ResetSettings => self.reset_settings(),
            Request::Shutdown => self.shutdown(),
            Request::Storage => self.storage(),
            Request::Clear => self.clear(),
            Request::Export { scope, destination, since, until, include_files } => {
                self.export_cache(scope, destination, since, until, include_files)
            }
            Request::Prunable { days } => match self.hub.store().prunable(now(), days) {
                Ok((messages, bytes)) => Response::Prunable { messages, bytes },
                Err(err) => internal(err),
            },
            Request::TestMicrophone { on, loopback } => match self.hub.calls.test_microphone(&self.hub, on, loopback) {
                Ok(()) => Response::Ok,
                Err(err) => call_error(err),
            },
            Request::TestSound => match self.hub.calls.test_sound(&self.hub) {
                Ok(()) => Response::Ok,
                Err(err) => call_error(err),
            },
            Request::NotifyTest => {
                if !crate::notify::enabled() {
                    return Response::NotifyTested { posted: false, detail: crate::notify::DISABLED.to_owned() };
                }
                let (tell, told) = std::sync::mpsc::channel();
                self.hub.notify(crate::notify::Event::Test(tell));
                match told.recv_timeout(Duration::from_secs(5)) {
                    Ok((posted, detail)) => Response::NotifyTested { posted, detail },
                    Err(_) => Response::NotifyTested { posted: false, detail: "Notifications are not running in tinyclient's background service.".to_owned() },
                }
            }
            Request::SaveLogs => {
                let mut paths = Vec::new();
                let Some(guard) = protocol::log_policy::current() else {
                    return Response::LogsSaved { paths };
                };
                let policy = guard.policy();
                if !policy.export_enabled() || !policy.ring_enabled() {
                    return Response::LogsSaved { paths };
                }
                if let Ok(Some(path)) = guard.export_bundle() {
                    paths.push(path.display().to_string());
                }
                if let Ok(protocol::MediaResponse::LogsSaved { path }) = self.hub.calls.request(&self.hub, &protocol::MediaRequest::SaveLogs) {
                    if !path.is_empty() {
                        paths.push(path);
                    }
                }
                Response::LogsSaved { paths }
            }
            Request::ActivityFeed { account, kind, cursor, limit } => {
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let limit = if limit == 0 { 20 } else { limit.min(50) };
                match self.ask(&account, |reply| Command::ActivityFeed { kind, cursor, limit, reply }) {
                    Ok(mut page) => {
                        for item in &mut page.items {
                            clip(&mut item.actor, 120);
                            clip(&mut item.text, 500);
                            clip(&mut item.emoji, 64);
                        }
                        Response::ActivityFeed { items: page.items, cursor: page.cursor }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::ActivityMarkRead { account, item_id } => {
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                match self.ask(&account, |reply| Command::ActivityMarkRead { item_id, reply }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::BookmarkAdd { conversation, title, link, emoji } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let conversation_id = conversation.id.clone();
                match self.ask(&account, |reply| Command::BookmarkAdd { conversation_id, title, link, emoji, reply }) {
                    Ok(bookmark) => {
                        self.hub.infos.forget(&conversation);
                        Response::Bookmark { bookmark }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::BookmarkEdit { conversation, bookmark_id, title, link, emoji } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let conversation_id = conversation.id.clone();
                match self.ask(&account, |reply| Command::BookmarkEdit { conversation_id, bookmark_id, title, link, emoji, reply }) {
                    Ok(bookmark) => {
                        self.hub.infos.forget(&conversation);
                        Response::Bookmark { bookmark }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::BookmarkRemove { conversation, bookmark_id } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let conversation_id = conversation.id.clone();
                match self.ask(&account, |reply| Command::BookmarkRemove { conversation_id, bookmark_id, reply }) {
                    Ok(()) => {
                        self.hub.infos.forget(&conversation);
                        Response::Ok
                    }
                    Err(refused) => *refused,
                }
            }
            Request::BrowseChannels { space, query, cursor, limit } => {
                let account = space.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let limit = if limit == 0 { 50 } else { limit.min(100) };
                match self.ask(&account, |reply| Command::BrowseChannels {
                    space_id: space.id,
                    query,
                    cursor,
                    limit,
                    reply,
                }) {
                    Ok(mut page) => {
                        for ch in &mut page.channels {
                            clip(&mut ch.name, 120);
                            clip(&mut ch.topic, 300);
                        }
                        Response::BrowsedChannels { channels: page.channels, cursor: page.cursor }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::PreviewChannel { conversation } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let conversation_id = conversation.id.clone();
                match self.ask(&account, |reply| Command::PreviewChannel { conversation_id, reply }) {
                    Ok(()) => match self.hub.store().conversation(&conversation) {
                        Ok(Some(row)) => Response::Conversation { conversation: self.row(row) },
                        Ok(None) => error(ErrorCode::NotFound, "no such conversation"),
                        Err(err) => internal(err),
                    },
                    Err(refused) => *refused,
                }
            }
            Request::Join { conversation } => {
                let account = conversation.account();
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let conversation_id = conversation.id.clone();
                match self.ask(&account, |reply| Command::Join { conversation_id, reply }) {
                    Ok(()) => {
                        self.hub.infos.forget(&conversation);
                        match self.hub.store().conversation(&conversation) {
                            Ok(Some(row)) => Response::Conversation { conversation: self.row(row) },
                            Ok(None) => Response::Ok,
                            Err(err) => internal(err),
                        }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::AccountFiles { account, query, kind, cursor, limit } => {
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                let limit = if limit == 0 { 25 } else { limit.min(100) };
                match self.ask(&account, |reply| Command::AccountFiles { query, kind, cursor, limit, reply }) {
                    Ok(mut page) => {
                        for item in &mut page.items {
                            clip(&mut item.title, 256);
                            clip(&mut item.url, 2_000);
                        }
                        Response::AccountFiles { items: page.items, cursor: page.cursor, more: page.more }
                    }
                    Err(refused) => *refused,
                }
            }
            Request::DeleteFile { account, file_id } => {
                if let Err(refused) = self.online(&account) {
                    return *refused;
                }
                match self.ask(&account, |reply| Command::DeleteFile { file_id, reply }) {
                    Ok(()) => Response::Ok,
                    Err(refused) => *refused,
                }
            }
            Request::MessageAction {
                conversation,
                message_id,
                block_id,
                action_id,
                element_type,
                value,
                values,
            } => self.act(&conversation, |conversation_id, reply| Command::MessageAction {
                conversation_id,
                message_id,
                block_id,
                action_id,
                element_type,
                value,
                values,
                reply,
            }),
            Request::PlaySound { sound } => {
                let known = sound != "none" && settings::sound_word(&sound) && match sound.strip_prefix("custom:") {
                    Some(id) => self.hub.settings().custom_sound(id).is_some(),
                    None => true,
                };
                if !known {
                    return error(ErrorCode::NotFound, "no such sound");
                }
                self.hub.notify(crate::notify::Event::Play(sound));
                Response::Ok
            }
        }
    }
}

/// Tiny keeps every server in the one item. A second sign-in is folded in before anything is
/// saved, so it does not replace the server already there. Other networks still replace.
fn merged(network: Network, session: &str) -> Result<String, String> {
    if network != Network::Tiny || !session::enabled() {
        return Ok(session.to_owned());
    }
    match session::read(Network::Tiny) {
        session::Stored::Absent => Ok(session.to_owned()),
        session::Stored::Unreadable(why) => {
            Err(format!("The session could not be read from {}: {why}", session::STORE))
        }
        session::Stored::Found(existing) => adapters::tiny::combine(&existing, session),
    }
}

/// Keeps the session a sign-in drawn by the shell ended with and starts its accounts, as
/// `Chat::sign_in` does with a helper's. Answers the accounts it started.
fn adopt(hub: &Arc<Hub>, adapters: &Adapters, network: Network, session: &str) -> Result<Vec<AccountKey>, String> {
    // Read and fold before stopping the adapters. A failed read leaves the signed-in server up.
    let session = merged(network, session)?;
    // A scratch directory keeps no session. The account still starts there, for a test.
    if session::enabled() {
        session::save(network, &session)?;
    }
    lock(adapters).retain(|account, _| account.network != network);
    let mut started = Vec::new();
    for signed in adapters::accounts(network, &session)? {
        let account = AccountKey {
            network,
            account: signed.account,
        };
        hub.store().set_account(&account, &signed.name, "").map_err(|err| err.to_string())?;
        let adapter = adapters::start(account.clone(), &signed.session, hub.clone())?;
        lock(adapters).insert(account.clone(), adapter);
        started.push(account);
    }
    hub.changed(Change::Status);
    Ok(started)
}

/// One connection's requests, in turn. A frame that does not parse (a shell of another
/// version) and an answer too large for a frame are answered in words, not by hanging up. A
/// request that panics is answered `Internal` and the connection goes on: the panic is logged
/// by the hook, and the locks it poisoned are taken anyway (`hub::lock`).
fn serve_connection(chat: &Chat, mut stream: ChatStream) -> io::Result<()> {
    loop {
        let bytes = match read_frame_bytes(&mut stream) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(err),
        };
        let answer = match serde_json::from_slice::<Request>(&bytes) {
            Ok(request) => {
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| chat.handle(request)));
                handled.unwrap_or_else(|_| error(ErrorCode::Internal, "tinyclient hit a problem answering this. It has been noted."))
            }
            Err(err) => error(ErrorCode::Unsupported, format!("chatd does not understand this request; tinyclient and chatd may be different versions ({err})")),
        };
        match write_frame(&mut stream, &answer) {
            Err(err) if err.kind() == ErrorKind::InvalidData => {
                write_frame(&mut stream, &error(ErrorCode::TooLarge, "The answer was too large. Ask for a smaller page."))?
            }
            written => written?,
        }
    }
}

/// Answers requests until the process is killed. A connection may send many requests, and a
/// slow one (a sign-in, a wait) holds up only its own connection. The sessions are read once
/// the socket is answering (`Chat::read_sessions`). A failed accept or thread spawn (out of
/// file descriptors) drops that one connection and goes on.
pub fn serve(listener: ChatListener, chat: Chat) -> io::Result<()> {
    let chat = Arc::new(chat);
    let _ = chat.this.set(Arc::downgrade(&chat));
    crate::notify::start(chat.hub.clone(), Arc::downgrade(&chat));
    chat.arm_queue();
    Chat::read_sessions(&chat, Network::ALL.to_vec(), true);
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!("chatd: accept: {err}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let chat = chat.clone();
        // Authentication requests can exceed 256 KiB in debug builds as the request/status
        // contract grows. Leave headroom so sign-in returns a reply instead of aborting chatd.
        let spawned = std::thread::Builder::new().name("connection".into()).stack_size(1024 * 1024).spawn(move || {
            if let Err(err) = serve_connection(&chat, stream) {
                tracing::warn!("chatd: connection: {err}");
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("chatd: connection thread: {err}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The name a file is sent under.
fn file_name(file: &protocol::OutgoingFile) -> String {
    if !file.name.is_empty() {
        return file.name.clone();
    }
    std::path::Path::new(&file.path).file_name().map_or_else(|| file.path.clone(), |name| name.to_string_lossy().into_owned())
}

/// What a file to be sent is, from its name: enough for the pending row to draw it.
fn kind_of(path: &str) -> MediaKind {
    let extension = std::path::Path::new(path).extension().map(|extension| extension.to_string_lossy().to_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => MediaKind::Image,
        "gif" => MediaKind::Gif,
        "mp4" | "mov" | "m4v" | "webm" => MediaKind::Video,
        "mp3" | "m4a" | "ogg" | "opus" | "wav" => MediaKind::Audio,
        _ => MediaKind::File,
    }
}

fn megabytes(bytes: u64) -> String {
    match bytes {
        bytes if bytes >= 1 << 30 => format!("{} GB", bytes >> 30),
        bytes => format!("{} MB", bytes >> 20),
    }
}

/// The files of an unsent message, to send again from where they were.
fn outgoing_files(message: &Message) -> Vec<protocol::OutgoingFile> {
    let ours = message.attachments.iter().filter(|attachment| attachment.file.id.starts_with("upload-"));
    ours.filter_map(|attachment| {
        Some(protocol::OutgoingFile {
            path: attachment.file.path.clone()?,
            name: attachment.name.clone(),
            spoiler: attachment.spoiler,
            as_file: false,
            original: false,
            view_once: attachment.view_once,
            media: None,
        })
    })
    .collect()
}

/// The card as the shell gets it: each value in its typed place, cut to its length. A line an
/// adapter still gives as a labelled `field` moves to its place when the label names one.
fn typed_profile(person: Person, mut about: About) -> Profile {
    let mut leftovers = Vec::new();
    for (label, value) in std::mem::take(&mut about.fields) {
        if label.is_empty() || value.is_empty() {
            continue;
        }
        let empty = |held: &String| held.is_empty();
        match label.to_lowercase().as_str() {
            "pronouns" if empty(&about.pronouns) => about.pronouns = value,
            "title" | "job title" if empty(&about.title) => about.title = value,
            "department" if empty(&about.department) => about.department = value,
            "nickname" if empty(&about.nickname) => about.nickname = value,
            "email" | "e-mail" => about.contacts.push(protocol::Contact { kind: protocol::ContactKind::Email, value, masked: false }),
            "phone" | "mobile" => about.contacts.push(protocol::Contact { kind: protocol::ContactKind::Phone, value, masked: true }),
            "roles" if about.roles.is_empty() => about.roles = value.split(", ").map(str::to_owned).collect(),
            // The handle is on the person already.
            "username" | "handle" if value.trim_start_matches('@') == person.handle.trim_start_matches('@') => {}
            _ => leftovers.push((label, value)),
        }
    }
    leftovers.truncate(protocol::PROFILE_FIELDS);
    for (label, value) in &mut leftovers {
        clip(label, 40);
        clip(value, 200);
    }
    for text in [&mut about.nickname, &mut about.pronouns, &mut about.title, &mut about.department, &mut about.away] {
        clip(text, 200);
    }
    clip(&mut about.bio, 1_000);
    clip(&mut about.note, 1_000);
    clip(&mut about.time_zone, 64);
    about.mutual_spaces.truncate(MUTUAL_SPACES);
    about.mutual_spaces.iter_mut().for_each(|title| clip(title, 100));
    about.roles.retain(|role| !role.is_empty());
    about.roles.truncate(protocol::PROFILE_ROLES);
    about.roles.iter_mut().for_each(|role| clip(role, 100));
    about.contacts.truncate(protocol::PROFILE_CONTACTS);
    for contact in &mut about.contacts {
        clip(&mut contact.value, 200);
        // Phone numbers are never drawn in full until clicked, whoever the network is.
        contact.masked |= contact.kind == protocol::ContactKind::Phone;
    }
    let mut profile = Profile::of(person);
    profile.bio = about.bio;
    profile.mutual_spaces = about.mutual_spaces;
    profile.time_zone = about.time_zone;
    profile.utc_offset = about.utc_offset;
    profile.fields = leftovers;
    profile.since = about.since;
    profile.mutual_friends = about.mutual_friends;
    profile.accent = about.accent;
    profile.banner = about.banner;
    profile.nickname = about.nickname;
    profile.pronouns = about.pronouns;
    profile.title = about.title;
    profile.department = about.department;
    profile.roles = about.roles;
    profile.contacts = about.contacts;
    profile.mutual_total = about.mutual_total;
    profile.joined = about.joined;
    profile.friends_since = about.friends_since;
    profile.last_seen = about.last_seen;
    profile.status_until = about.status_until;
    profile.away = about.away;
    profile.away_until = about.away_until;
    profile.note = about.note;
    profile.warning = about.warning;
    profile
}

/// Slack `{channel}:{ts}` and Teams `{channel};messageid={root}`, when neither part is itself
/// a thread id. A Discord id is a channel and is not derived.
fn derivable_thread(conversation: &Key) -> Option<(String, String)> {
    match conversation.network {
        Network::Slack => {
            let (channel, root) = conversation.id.split_once(':')?;
            if channel.is_empty() || root.is_empty() || root.contains(':') {
                return None;
            }
            Some((channel.to_owned(), root.to_owned()))
        }
        Network::Teams => {
            let (channel, root) = conversation.id.split_once(";messageid=")?;
            if channel.is_empty() || root.is_empty() || root.contains(";messageid=") {
                return None;
            }
            Some((channel.to_owned(), root.to_owned()))
        }
        _ => None,
    }
}

/// The first non-empty line, cut to `most` characters with an ellipsis when it ran over.
fn first_line(text: &str, most: usize) -> String {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    if first.chars().count() > most {
        format!("{}…", first.chars().take(most).collect::<String>().trim_end())
    } else {
        first.to_owned()
    }
}

/// A Discord thread name: whitespace flattened, at most 100 characters, never empty.
fn thread_name(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = flat.chars().take(100).collect();
    let name = name.trim().to_owned();
    if name.is_empty() { "Thread".to_owned() } else { name }
}

/// The link the shell uses to decide a network is on the rail. A live connection wins. When the
/// adapter has not reported one, an account kept on screen (its session is unread, refused, or
/// still in the keychain) stays as offline instead of disappearing.
fn published_link(live: Link, kept: impl IntoIterator<Item = Link>) -> Link {
    if live != Link::SignedOut {
        return live;
    }
    if kept.into_iter().any(|link| link != Link::SignedOut) {
        Link::Offline
    } else {
        Link::SignedOut
    }
}

/// What `messages` answers when the account must not show its cache. Tree and Conversation
/// already refuse with NotFound. An empty Messages page would clear the open pane.
fn hidden_history() -> Response {
    error(ErrorCode::NotFound, "signed out")
}

/// The lab's signed-out-messages driver. A signed-out history read is a refusal, not an empty page.
pub fn prove_signed_out_messages() -> Result<(), String> {
    match hidden_history() {
        Response::Error { code, detail, .. } if code == ErrorCode::NotFound && detail.contains("signed out") => Ok(()),
        other => Err(format!("hidden history was {other:?}, not a signed-out refusal")),
    }
}

/// What Threads and Topics answer when the account must not show its cache. Tree already
/// refuses with NotFound. An empty Conversations page looks like a real empty list.
fn hidden_list() -> Response {
    error(ErrorCode::NotFound, "signed out")
}

/// The lab's signed-out-threads driver. A signed-out Threads/Topics read is a refusal, not an empty list.
pub fn prove_signed_out_threads() -> Result<(), String> {
    match hidden_list() {
        Response::Error { code, detail, .. } if code == ErrorCode::NotFound && detail.contains("signed out") => Ok(()),
        other => Err(format!("hidden list was {other:?}, not a signed-out refusal")),
    }
}

/// Whether a page opened around one message should ask the network. A full window that holds
/// the anchor usually should not. It should when a row is blank plaintext: an encrypted
/// message stored without the flag looks like that, and the cache cannot see it.
fn around_needs_catchup(older: bool, newer: bool, anchor_present: bool, blank: bool) -> bool {
    !older || !newer || !anchor_present || blank
}

/// `before` excludes that row. The cursor that still includes the last row of the open window
/// is the first stored message after it. At the tip there is no such message.
fn around_followup(successor: Option<&str>) -> Option<&str> {
    successor.filter(|id| !id.is_empty())
}

/// Cursors for a page scrolled up. `before` excludes that id, so the anchor is the page on
/// screen and the oldest row is the next older page. A blank window asks for the anchor even
/// when older rows are already cached. The same id is asked once. An empty oldest id is the anchor.
fn before_cursors<'a>(anchor: &'a str, oldest: Option<&'a str>, blank: bool, no_older: bool) -> Vec<&'a str> {
    let mut cursors = Vec::new();
    if blank {
        cursors.push(anchor);
    }
    if no_older {
        let edge = oldest.filter(|id| !id.is_empty()).unwrap_or(anchor);
        if !cursors.contains(&edge) {
            cursors.push(edge);
        }
    }
    cursors
}

/// A stored row with no words and no file. A captionless photo is empty text on purpose.
fn window_has_blank(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        message.seal == protocol::Seal::Plain && !message.deleted && message.attachments.is_empty() && message.text.trim().is_empty()
    })
}

/// The lab's before-blank driver. A blank scrolled page asks for the rows on screen.
/// A page of real words that already has older rows does not. The next older page still asks.
pub fn prove_before_blank() -> Result<(), &'static str> {
    let on_screen = before_cursors("anchor", Some("oldest"), true, false) == ["anchor"];
    let extend = before_cursors("anchor", Some("oldest"), false, true) == ["oldest"];
    let quiet = before_cursors("anchor", Some("oldest"), false, false).is_empty();
    if on_screen && extend && quiet {
        Ok(())
    } else {
        Err("a blank scrolled page would not be caught up")
    }
}

/// The lab's around-blank driver. A full window of blank rows still asks. A window of words does not.
pub fn prove_around_blank() -> Result<(), &'static str> {
    let asks = around_needs_catchup(true, true, true, true);
    let keeps = !around_needs_catchup(true, true, true, false);
    let photo = {
        let mut photo = Message::plain("m", "Ada", "", 1, false);
        photo.attachments.push(protocol::Attachment::new("pic", protocol::MediaFile::new("f", protocol::MediaKind::Image)));
        !window_has_blank(&[photo])
    };
    let blank = window_has_blank(&[Message::plain("m", "Ada", "", 1, false)]);
    if asks && keeps && photo && blank {
        Ok(())
    } else {
        Err("a full window of blank rows would not be caught up")
    }
}

/// A history catch-up. Anything else asked before the adapter exists is not replayed:
/// a call leave, a send, or a search has no one waiting on the stored page.
fn kept_without_adapter(command: &Command) -> bool {
    matches!(
        command,
        Command::Backfill(_) | Command::History { .. } | Command::HistoryAfter { .. } | Command::HistoryAround { .. }
    )
}

/// The lab's kept-backfill driver. A catch-up asked before the adapter exists is kept.
pub fn prove_kept_backfill() -> Result<(), &'static str> {
    if kept_without_adapter(&Command::Backfill("c".into())) && !kept_without_adapter(&Command::CallLeave("c".into())) {
        Ok(())
    } else {
        Err("a backfill before the adapter exists would be dropped")
    }
}

/// Marks every stored account of `network` Offline with `why`.
fn mark_network_offline(hub: &crate::hub::Hub, network: Network, why: &str) {
    let Ok(rows) = hub.store().accounts() else {
        return;
    };
    for row in rows.into_iter().filter(|row| row.key.network == network) {
        hub.set_link(&row.key, Link::Offline, why);
    }
}

/// The lab's connect-keeps-offline driver. A connect failure marks cached accounts Offline.
pub fn prove_connect_keeps_offline() -> Result<(), String> {
    let hub = std::sync::Arc::new(crate::hub::Hub::new(
        crate::store::Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?,
    ));
    let account = protocol::AccountKey {
        network: Network::Tiny,
        account: "11111111-1111-1111-1111-111111111111".into(),
    };
    hub.store()
        .set_account(&account, "Ada", "")
        .map_err(|err| err.to_string())?;
    mark_network_offline(&hub, Network::Tiny, "connect failed");
    match hub.link(&account) {
        Link::Offline => Ok(()),
        other => Err(format!("link was {other:?}, not Offline")),
    }
}

/// The lab's offline-queue driver. Text written while the account is offline is stored and
/// handed off in order when it connects. A file is refused. One older than a day fails instead
/// of going. A restart does not fail a young queued row, and does not send an old one.
pub fn prove_offline_queue() -> Result<(), String> {
    let mut store = Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?;
    let account = AccountKey { network: Network::Discord, account: "ada".into() };
    store
        .upsert_conversation(&account, &crate::store::ConversationRow::new("general", ConversationKind::Channel, "general"))
        .map_err(|err| err.to_string())?;
    let chat = Arc::new(Chat::new(store));
    chat.arm_queue();
    let conversation = Key::new(&account, "general");
    chat.hub.set_link(&account, Link::Offline, "");
    let send = |id: &str, text: &str, with: Outgoing| {
        chat.handle(Request::Send {
            conversation: conversation.clone(),
            client_id: id.into(),
            text: text.into(),
            reply_to: None,
            with,
        })
    };
    let Response::Sent { message } = send("1", "one", Outgoing::default()) else {
        return Err("offline text was refused".into());
    };
    if !message.waiting || message.state != SendState::Pending {
        return Err("a queued send was marked on its way".into());
    }
    let Response::Sent { message } = send("2", "two", Outgoing::default()) else {
        return Err("the second offline text was refused".into());
    };
    if !message.waiting || message.text != "two" {
        return Err("the second offline text was not queued".into());
    }
    let files = send(
        "3",
        "pic",
        Outgoing {
            files: vec![protocol::OutgoingFile {
                path: "pic.png".into(),
                name: "pic.png".into(),
                spoiler: false,
                as_file: false,
                original: false,
                view_once: false,
                media: None,
            }],
            ..Outgoing::default()
        },
    );
    let Response::Error { code: ErrorCode::Offline, detail } = files else {
        return Err("a file send while offline was not refused".into());
    };
    if !detail.contains("Files") {
        return Err(detail);
    }
    if chat.hub.store().message(&conversation, "3").map_err(|err| err.to_string())?.is_some() {
        return Err("a file was stored offline".into());
    }
    let scheduled = send("4", "later", Outgoing { schedule: Some(1), ..Outgoing::default() });
    if !matches!(scheduled, Response::Error { code: ErrorCode::Offline, .. }) {
        return Err("a scheduled send while offline was stored".into());
    }
    let edit = chat.handle(Request::Edit { conversation: conversation.clone(), message_id: "1".into(), text: "no".into(), spans: Vec::new() });
    let react = chat.handle(Request::React {
        conversation: conversation.clone(),
        message_id: "1".into(),
        emoji: "👍".into(),
        on: true,
        burst: false,
    });
    let delete = chat.handle(Request::Delete { conversation: conversation.clone(), message_id: "1".into(), scope: protocol::DeleteScope::Everyone });
    if !matches!(edit, Response::Error { code: ErrorCode::Offline, .. })
        || !matches!(react, Response::Error { code: ErrorCode::Offline, .. })
        || !matches!(delete, Response::Error { code: ErrorCode::Offline, .. })
    {
        return Err("an edit, reaction, or delete was accepted offline".into());
    }
    let Response::Sent { message } = send("hold", "hold", Outgoing::default()) else {
        return Err("the cancellable send was refused".into());
    };
    if !message.waiting {
        return Err("the cancellable send was not queued".into());
    }
    let cancelled = chat.handle(Request::CancelSend { conversation: conversation.clone(), client_id: "hold".into() });
    if !matches!(cancelled, Response::Ok) {
        return Err("cancel of a queued send was refused".into());
    }
    if chat.hub.store().message(&conversation, "hold").map_err(|err| err.to_string())?.is_some() {
        return Err("a cancelled queued send was kept".into());
    }
    chat.hub.set_link(&account, Link::SignedOut, "");
    let signed_out = send("ghost", "ghost", Outgoing::default());
    if !matches!(signed_out, Response::Error { code: ErrorCode::Offline, .. }) {
        return Err("a signed-out send was stored".into());
    }
    chat.hub.set_link(&account, Link::Expired, "");
    let expired = send("expired", "expired", Outgoing::default());
    if !matches!(expired, Response::Error { code: ErrorCode::Offline, .. }) {
        return Err("an expired account stored a send".into());
    }
    chat.hub.set_link(&account, Link::Offline, "");
    let kept = chat.hub.store().message(&conversation, "1").map_err(|err| err.to_string())?.ok_or("queued text was dropped")?;
    let next = chat.hub.store().message(&conversation, "2").map_err(|err| err.to_string())?.ok_or("second queued text was dropped")?;
    if kept.text != "one" || next.text != "two" || kept.sent > next.sent {
        return Err("the queue lost the order they were written".into());
    }
    for n in 3..=32 {
        let Response::Sent { message } = send(&n.to_string(), "more", Outgoing::default()) else {
            return Err(format!("queue slot {n} was refused"));
        };
        if !message.waiting {
            return Err(format!("queue slot {n} was marked on its way"));
        }
    }
    let over = send("33", "more", Outgoing::default());
    if !matches!(over, Response::Error { code: ErrorCode::Refused, .. }) {
        return Err("the queue grew past 32".into());
    }
    if chat.hub.store().message(&conversation, "33").map_err(|err| err.to_string())?.is_some() {
        return Err("the refused extra was stored".into());
    }
    let mut late = chat.hub.store().message(&conversation, "1").map_err(|err| err.to_string())?.ok_or("missing late row")?;
    late.sent = now() - QUEUE_AGE - 120;
    chat.hub.store().ingest(&conversation, &late, false).map_err(|err| err.to_string())?;
    let mut young = chat.hub.store().message(&conversation, "2").map_err(|err| err.to_string())?.ok_or("missing young row")?;
    young.sent = now() - 90;
    chat.hub.store().ingest(&conversation, &young, false).map_err(|err| err.to_string())?;
    chat.hub.store().fail_pending(i64::MAX, "restart").map_err(|err| err.to_string())?;
    let still = chat.hub.store().message(&conversation, "1").map_err(|err| err.to_string())?.ok_or("restart dropped the queue")?;
    if !still.waiting || still.state != SendState::Pending {
        return Err("a restart failed a queued send".into());
    }
    let _ = chat.handle(Request::Messages { conversation: conversation.clone(), page: Page::Newest, limit: 50 });
    let late = chat.hub.store().message(&conversation, "1").map_err(|err| err.to_string())?.ok_or("the late row vanished")?;
    let young = chat.hub.store().message(&conversation, "2").map_err(|err| err.to_string())?.ok_or("the young row vanished")?;
    if late.waiting || !matches!(late.state, SendState::Failed { reason } if reason == QUEUE_LATE) {
        return Err("a day-old queued send was not failed".into());
    }
    if !young.waiting || young.state != SendState::Pending {
        return Err("a queued send younger than a day was failed while still offline".into());
    }
    let queued = chat.hub.store().queued(&account).map_err(|err| err.to_string())?;
    if queued.first().map(|(_, message)| message.id.as_str()) != Some("2") {
        return Err("the oldest queued send was not first".into());
    }
    let mut previous = 0;
    for (_, message) in &queued {
        if message.sent < previous {
            return Err("the queue lost the order they were written".into());
        }
        previous = message.sent;
    }
    let handed = now();
    chat.hub.set_link(&account, Link::Connected, "");
    let late = chat.hub.store().message(&conversation, "1").map_err(|err| err.to_string())?.ok_or("the late row vanished on connect")?;
    let young = chat.hub.store().message(&conversation, "2").map_err(|err| err.to_string())?.ok_or("the young row vanished on connect")?;
    let third = chat.hub.store().message(&conversation, "3").map_err(|err| err.to_string())?.ok_or("a later queued send vanished")?;
    if late.waiting || late.sent >= handed || !matches!(late.state, SendState::Failed { .. }) {
        return Err("a day-old send went when the link returned".into());
    }
    if young.waiting || young.state != SendState::Pending || young.sent < handed || young.text != "two" {
        return Err("a young queued send was not handed off".into());
    }
    if third.waiting || third.state != SendState::Pending || third.sent < handed {
        return Err("a later queued send was not handed off".into());
    }
    Ok(())
}

/// The lab's space-mark-all-read driver. Reading one Space out leaves every other Space's and
/// every direct row's badge alone, and a frame from an older peer without the Space still
/// reads the whole Inbox.
pub fn prove_space_mark_all_read() -> Result<(), String> {
    match serde_json::from_value::<Request>(serde_json::json!({"op": "mark_all_read"})) {
        Ok(Request::MarkAllRead { network: None, space: None }) => {}
        _ => return Err("an older frame without a Space did not read".into()),
    }
    let mut store = Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?;
    let account = AccountKey { network: Network::Discord, account: "ada".into() };
    let mut room = crate::store::ConversationRow::new("general", ConversationKind::Channel, "general");
    room.space = Some("4000".into());
    store.upsert_conversation(&account, &room).map_err(|err| err.to_string())?;
    store
        .upsert_conversation(&account, &crate::store::ConversationRow::new("elsewhere", ConversationKind::Channel, "elsewhere"))
        .map_err(|err| err.to_string())?;
    let general = Key::new(&account, "general");
    let elsewhere = Key::new(&account, "elsewhere");
    store.ingest(&general, &Message::plain("8", "mara", "for this space", 10, false), true).map_err(|err| err.to_string())?;
    store.ingest(&elsewhere, &Message::plain("9", "mara", "for another", 20, false), true).map_err(|err| err.to_string())?;
    let chat = Arc::new(Chat::new(store));
    let read = chat.handle(Request::MarkAllRead { network: Some(Network::Discord), space: Some(Key::new(&account, "4000")) });
    if !matches!(read, Response::Ok) {
        return Err(format!("the space-scoped read was refused: {read:?}"));
    }
    let row = |chat: &Chat, key: &Key| -> Result<Conversation, String> {
        chat.hub.store().conversation(key).map_err(|err| err.to_string())?.ok_or_else(|| "the row vanished".into())
    };
    if row(&chat, &general)?.unread != 0 {
        return Err("a conversation of the Space kept its badge".into());
    }
    if row(&chat, &elsewhere)?.unread != 1 {
        return Err("another Space's badge was cleared".into());
    }
    Ok(())
}

/// Who we are on this conversation's account for a pending outgoing row: a prior message of
/// ours in this chat when one names us (per-chat nick/avatar), else the cached self person.
/// Never the account or workspace label. "You" only while nothing of us is cached.
fn pending_self_sender(store: &Store, conversation: &Key) -> Sender {
    let account = conversation.account();
    let row = match store.accounts() {
        Ok(rows) => rows.into_iter().find(|row| row.key == account),
        Err(_) => None,
    };
    let Some(row) = row.filter(|row| !row.me.is_empty()) else {
        return Sender { name: "You".into(), ..Sender::default() };
    };
    let me = row.me.clone();
    let contextual = store.recent_own_sender(conversation, &me).ok().flatten();
    let person = store.person(&Key::new(&account, &me)).ok().flatten();
    let name = contextual
        .as_ref()
        .map(|sender| sender.name.as_str())
        .filter(|name| !name.is_empty() && *name != "You")
        .or_else(|| person.as_ref().map(|person| person.name.as_str()).filter(|name| !name.is_empty()))
        .unwrap_or("You")
        .to_owned();
    let avatar = contextual
        .and_then(|sender| sender.avatar)
        .or_else(|| person.and_then(|person| person.avatar))
        .or(row.avatar);
    Sender { id: me, name, avatar, bot: false }
}

/// Seeds one account's self person with a picture already on the media row.
fn seed_self(
    store: &mut Store,
    account: &AccountKey,
    label: &str,
    me: &str,
    name: &str,
    avatar_id: &str,
) -> Result<(), String> {
    store.set_account(account, label, me).map_err(|err| err.to_string())?;
    let avatar = MediaFile::new(avatar_id, MediaKind::Avatar);
    store
        .register_media(account, &[(avatar.clone(), format!("https://cdn.example/{avatar_id}.png"))])
        .map_err(|err| err.to_string())?;
    store
        .upsert_people(
            account,
            &[crate::store::PersonRow {
                id: me.to_owned(),
                name: name.to_owned(),
                handle: String::new(),
                avatar: Some(avatar_id.to_owned()),
                bot: false,
                status: String::new(),
                link: String::new(),
            }],
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// The lab's pending-message-identity driver. A pending Send names the cached self sender for
/// that conversation's account before any network acknowledgement, keeps accounts isolated, and
/// falls back safely when nothing of us is cached.
pub fn prove_pending_message_identity() -> Result<(), String> {
    let mut store = Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?;
    let first = AccountKey { network: Network::Discord, account: "acct-a".into() };
    let second = AccountKey { network: Network::Discord, account: "acct-b".into() };
    let same_id = AccountKey { network: Network::Slack, account: "TWORKSPACE".into() };
    let other_net = AccountKey { network: Network::Slack, account: "TOTHER".into() };
    seed_self(&mut store, &first, "Moss Server", "user-1", "Ada Vale", "ava-ada")?;
    seed_self(&mut store, &second, "Fern Server", "user-1", "Bea Vale", "ava-bea")?;
    seed_self(&mut store, &same_id, "Harbor Labs", "U04ME", "Ada Harbor", "ava-harbor")?;
    seed_self(&mut store, &other_net, "Fernhill", "U04ME", "Ada Fern", "ava-fern")?;
    store
        .upsert_conversation(&first, &crate::store::ConversationRow::new("c-a", ConversationKind::Channel, "general"))
        .map_err(|err| err.to_string())?;
    store
        .upsert_conversation(&second, &crate::store::ConversationRow::new("c-b", ConversationKind::Channel, "general"))
        .map_err(|err| err.to_string())?;
    store
        .upsert_conversation(&same_id, &crate::store::ConversationRow::new("C0GEN", ConversationKind::Channel, "general"))
        .map_err(|err| err.to_string())?;
    store
        .upsert_conversation(&other_net, &crate::store::ConversationRow::new("C0GEN", ConversationKind::Channel, "general"))
        .map_err(|err| err.to_string())?;

    let nick_chat = Key::new(&first, "c-a");
    let mut prior = Message::plain("prior-1", "Ada Nick", "earlier", 1_700_000_000, true);
    prior.sender.id = "user-1".into();
    prior.sender.avatar = Some(MediaFile::new("ava-nick", MediaKind::Avatar));
    store.ingest(&nick_chat, &prior, false).map_err(|err| err.to_string())?;
    store.register_media(&first, &[(MediaFile::new("ava-nick", MediaKind::Avatar), "https://cdn.example/nick.png".into())])
        .map_err(|err| err.to_string())?;
    let fixture_dir = support_dir().join("pending-message-identity");
    std::fs::create_dir_all(&fixture_dir).map_err(|err| err.to_string())?;
    let nick_path = fixture_dir.join("nick.png");
    std::fs::write(&nick_path, vec![0u8; 321]).map_err(|err| err.to_string())?;
    let nick_path = nick_path.to_string_lossy().into_owned();
    store.media_landed(&first, "ava-nick", &nick_path, 321, now()).map_err(|err| err.to_string())?;

    let from_nick = pending_self_sender(&store, &nick_chat);
    if from_nick.id != "user-1" || from_nick.name != "Ada Nick" {
        return Err(format!("context sender was {from_nick:?}, not Ada Nick"));
    }
    if from_nick.avatar.as_ref().map(|file| file.id.as_str()) != Some("ava-nick") {
        return Err(format!("context avatar was {:?}, not ava-nick", from_nick.avatar));
    }
    if from_nick.avatar.as_ref().and_then(|file| file.path.as_deref()) != Some(nick_path.as_str()) {
        return Err("context avatar lost its already-landed local path".into());
    }

    let from_person = pending_self_sender(&store, &Key::new(&second, "c-b"));
    if from_person.id != "user-1" || from_person.name != "Bea Vale" || from_person.name.contains("Fern") {
        return Err(format!("second account sender was {from_person:?}; used workspace label or first account"));
    }
    if from_person.avatar.as_ref().map(|file| file.id.as_str()) != Some("ava-bea") {
        return Err(format!("second account avatar was {:?}, not ava-bea", from_person.avatar));
    }

    let harbor = pending_self_sender(&store, &Key::new(&same_id, "C0GEN"));
    let fern = pending_self_sender(&store, &Key::new(&other_net, "C0GEN"));
    if harbor.name != "Ada Harbor" || fern.name != "Ada Fern" || harbor.avatar == fern.avatar {
        return Err(format!("same own id across accounts leaked: {harbor:?} / {fern:?}"));
    }
    if harbor.name == "Harbor Labs" || fern.name == "Fernhill" {
        return Err(format!("workspace label used as author: {harbor:?} / {fern:?}"));
    }

    let unknown = AccountKey { network: Network::Teams, account: "lonely".into() };
    store.set_account(&unknown, "Tenant Label", "").map_err(|err| err.to_string())?;
    store
        .upsert_conversation(&unknown, &crate::store::ConversationRow::new("chat", ConversationKind::Direct, "Notes"))
        .map_err(|err| err.to_string())?;
    let fallback = pending_self_sender(&store, &Key::new(&unknown, "chat"));
    if fallback.id != "" || fallback.name != "You" || fallback.avatar.is_some() {
        return Err(format!("unknown profile fallback was {fallback:?}, not You"));
    }

    // The shared Send constructor fills the same sender before any acknowledgement.
    let chat = Chat::new(store);
    let pending = chat.new_message(&nick_chat, "client-pending".into(), "on it".into(), None, &Outgoing::default());
    if pending.client_id.as_deref() != Some("client-pending") {
        return Err(format!("pending client_id was {:?}", pending.client_id));
    }
    if pending.sender != from_nick || pending.shown_sender() != "Ada Nick" {
        return Err(format!(
            "new_message sender was {:?} (shown {}), expected {from_nick:?}",
            pending.sender,
            pending.shown_sender()
        ));
    }
    Ok(())
}

/// The lab's edit-history driver. An edited message keeps the time the network gave it.
/// A live edit does not store the words it replaced. A wording the network itself kept is
/// answered by `Revisions`.
pub fn prove_edit_history() -> Result<(), String> {
    let mut store = Store::open(std::path::Path::new(":memory:")).map_err(|err| err.to_string())?;
    let account = AccountKey { network: Network::Signal, account: "me".into() };
    store
        .upsert_conversation(&account, &crate::store::ConversationRow::new("chat", ConversationKind::Direct, "Ada"))
        .map_err(|err| err.to_string())?;
    let key = Key::new(&account, "chat");
    let mut message = Message::plain("m1", "Ada", "revised", 1_700_000_100, false);
    message.edited = true;
    message.edited_at = Some(1_700_000_200);
    store.ingest(&key, &message, false).map_err(|err| err.to_string())?;
    message.text = "revised again".into();
    message.edited_at = Some(1_700_000_300);
    store.ingest(&key, &message, false).map_err(|err| err.to_string())?;
    if !store.revisions(&key, "m1").map_err(|err| err.to_string())?.is_empty() {
        return Err("a live edit stored text the sender replaced".into());
    }
    store.note_revision(&key, "m1", "original", 1_700_000_000).map_err(|err| err.to_string())?;
    let chat = Chat::new(store);
    // A scratch directory shows seeded rows. A normal test process does not, until the account is connected.
    chat.hub.set_link(&account, Link::Connected, "");
    let shown = chat
        .hub
        .store()
        .message(&key, "m1")
        .map_err(|err| err.to_string())?
        .ok_or("the edited message was not stored")?;
    if shown.text != "revised again" || shown.edited_at != Some(1_700_000_300) || !shown.edited {
        return Err("the edited message lost its time".into());
    }
    match chat.handle(Request::Revisions { conversation: key, message_id: "m1".into() }) {
        Response::Revisions { revisions, .. }
            if revisions.len() == 1 && revisions[0].text == "original" && revisions[0].at == 1_700_000_000 =>
        {
            Ok(())
        }
        Response::Revisions { .. } => Err("revisions did not answer the earlier text".into()),
        _ => Err("revisions was not answered".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::published_link;
    use crate::adapters::Command;
    use crate::store::{ConversationRow, PersonRow, Store};
    use protocol::{AccountKey, ConversationKind, Key, Link, MediaFile, MediaKind, Message, Network, Outgoing};

    #[test]
    fn a_missing_error_names_the_call() {
        let words = super::response_detail(&protocol::Response::Ok);
        assert!(words.starts_with("Couldn't place that call"), "{words}");
        assert!(!words.contains("That didn't work"), "{words}");
    }

    #[test]
    fn a_pending_message_names_the_cached_self_sender_before_any_ack() {
        assert_eq!(super::prove_pending_message_identity(), Ok(()));
    }

    #[test]
    fn an_edited_message_answers_its_time_and_only_a_noted_earlier_wording() {
        assert_eq!(super::prove_edit_history(), Ok(()));
    }

    #[test]
    fn a_pending_sender_prefers_this_chat_then_the_person_and_never_the_account_label() {
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let account = AccountKey { network: Network::Tiny, account: "night".into() };
        store.set_account(&account, "Night Shift", "me-1").unwrap();
        store
            .register_media(&account, &[(MediaFile::new("face", MediaKind::Avatar), "https://cdn.example/face.png".into())])
            .unwrap();
        store
            .upsert_people(
                &account,
                &[PersonRow {
                    id: "me-1".into(),
                    name: "Robin Vale".into(),
                    handle: String::new(),
                    avatar: Some("face".into()),
                    bot: false,
                    status: String::new(),
                    link: String::new(),
                }],
            )
            .unwrap();
        store
            .upsert_conversation(&account, &ConversationRow::new("room", ConversationKind::Channel, "ops"))
            .unwrap();
        let room = Key::new(&account, "room");
        let person = super::pending_self_sender(&store, &room);
        assert_eq!((person.id.as_str(), person.name.as_str()), ("me-1", "Robin Vale"));
        assert_eq!(person.avatar.as_ref().map(|file| file.id.as_str()), Some("face"));

        let mut prior = Message::plain("p1", "Ops Nick", "hi", 10, true);
        prior.sender.id = "me-1".into();
        store.ingest(&room, &prior, false).unwrap();
        let nick = super::pending_self_sender(&store, &room);
        assert_eq!(nick.name, "Ops Nick");

        let chat = super::Chat::new(store);
        let pending = chat.new_message(&room, "c1".into(), "going".into(), None, &Outgoing::default());
        assert_eq!(pending.sender.name, "Ops Nick");
        assert_eq!(pending.sender.id, "me-1");
        assert_eq!(pending.shown_sender(), "Ops Nick");
        assert_ne!(pending.sender.name, "Night Shift");
    }

    #[test]
    fn a_tiny_pending_sender_is_found_by_the_user_id_and_never_by_the_email() {
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        // A Tiny account is keyed by the user id, and its own rows name that id as the sender.
        let account = AccountKey { network: Network::Tiny, account: "user-1".into() };
        store.set_account(&account, "tiny.example", "user-1").unwrap();
        store
            .upsert_conversation(&account, &ConversationRow::new("direct", ConversationKind::Direct, "Bea"))
            .unwrap();
        let direct = Key::new(&account, "direct");
        let mut earlier = Message::plain("m1", "Ada Vale", "hi", 10, true);
        earlier.sender.id = "user-1".into();
        store.ingest(&direct, &earlier, false).unwrap();
        let found = super::pending_self_sender(&store, &direct);
        assert_eq!((found.id.as_str(), found.name.as_str()), ("user-1", "Ada Vale"));

        // With the email stored as the account's own id nothing matches: the row read "You".
        store.set_account(&account, "tiny.example", "ada@example.com").unwrap();
        let lost = super::pending_self_sender(&store, &direct);
        assert_eq!(lost.name, "You");
    }

    #[test]
    fn a_pending_context_avatar_has_its_accounts_landed_path_before_ack() {
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let first = AccountKey { network: Network::Slack, account: "first".into() };
        let second = AccountKey { network: Network::Slack, account: "second".into() };
        let fixture_dir = protocol::support_dir().join("pending-avatar-paths");
        std::fs::create_dir_all(&fixture_dir).unwrap();
        for (account, file, bytes) in [(&first, "first.png", 123), (&second, "second.png", 456)] {
            super::seed_self(&mut store, account, "Workspace", "me", "Robin", "avatar").unwrap();
            store.upsert_conversation(account, &ConversationRow::new("chat", ConversationKind::Channel, "general")).unwrap();
            let mut previous = Message::plain("previous", "Chat Nick", "earlier", 10, true);
            previous.sender.id = "me".into();
            previous.sender.avatar = Some(MediaFile::new("avatar", MediaKind::Avatar));
            store.ingest(&Key::new(account, "chat"), &previous, false).unwrap();
            // The image landed after the message body was stored with no local path.
            let path = fixture_dir.join(file);
            std::fs::write(&path, vec![0u8; bytes as usize]).unwrap();
            store.media_landed(account, "avatar", &path.to_string_lossy(), bytes, crate::hub::now()).unwrap();
        }
        let chat = super::Chat::new(store);
        for (account, file, bytes) in [(&first, "first.png", 123), (&second, "second.png", 456)] {
            let pending = chat.new_message(&Key::new(account, "chat"), "pending".into(), "going".into(), None, &Outgoing::default());
            assert_eq!(pending.sender.name, "Chat Nick");
            let avatar = pending.sender.avatar.unwrap();
            let path = fixture_dir.join(file).to_string_lossy().into_owned();
            assert_eq!(avatar.path.as_deref(), Some(path.as_str()));
            assert!(std::path::Path::new(&path).is_file());
            assert_eq!(avatar.bytes, bytes);
        }
    }

    #[test]
    fn a_failed_connect_marks_cached_accounts_offline_so_chats_still_show() {
        let hub = std::sync::Arc::new(crate::hub::Hub::new(
            crate::store::Store::open(std::path::Path::new(":memory:")).unwrap(),
        ));
        let account = AccountKey {
            network: Network::Tiny,
            account: "11111111-1111-1111-1111-111111111111".into(),
        };
        hub.store().set_account(&account, "Ada", "").unwrap();
        super::mark_network_offline(&hub, Network::Tiny, "connect failed");
        assert_eq!(hub.link(&account), Link::Offline);
        assert_eq!(super::prove_connect_keeps_offline(), Ok(()));
    }

    /// Opening the newest page asks for a catch-up. If that ask arrives before the adapter
    /// is in the map, it has to be kept: the later list refresh is not a message change, so
    /// the pane does not ask again. A call leave is not a catch-up and is not kept.
    #[test]
    fn a_backfill_is_kept_until_the_adapter_exists() {
        assert!(super::kept_without_adapter(&Command::Backfill("c".into())));
        assert!(super::kept_without_adapter(&Command::History {
            conversation_id: "c".into(),
            before: "m".into(),
        }));
        assert!(super::kept_without_adapter(&Command::HistoryAfter {
            conversation_id: "c".into(),
            after: "m".into(),
        }));
        assert!(super::kept_without_adapter(&Command::HistoryAround {
            conversation_id: "c".into(),
            around: "m".into(),
        }));
        assert!(!super::kept_without_adapter(&Command::CallLeave("c".into())));
    }

    /// A chat with a full cache around the read marker does not ask again. A window of blank
    /// plaintext does: those rows were an encrypted message stored with no flag, and the cache
    /// cannot see that. A captionless photo is empty text on purpose and is not one of them.
    #[test]
    fn a_full_window_of_blank_rows_still_asks() {
        assert!(super::around_needs_catchup(true, true, true, true));
        assert!(!super::around_needs_catchup(true, true, true, false));
        assert!(super::around_needs_catchup(true, true, false, false));
        let mut photo = protocol::Message::plain("m", "Ada", "", 1, false);
        photo.attachments.push(protocol::Attachment::new("pic", protocol::MediaFile::new("f", protocol::MediaKind::Image)));
        assert!(!super::window_has_blank(&[photo]));
        assert!(super::window_has_blank(&[protocol::Message::plain("m", "Ada", "  ", 1, false)]));
    }

    /// The server's `before` cursor excludes that message. The page that still includes the
    /// last row of the open window is the one before the next stored message. At the tip there
    /// is no next message, and the newest page is what covers the window.
    #[test]
    fn the_page_that_covers_the_window_starts_after_its_last_row() {
        assert_eq!(super::around_followup(Some("next")), Some("next"));
        assert_eq!(super::around_followup(None), None);
        assert_eq!(super::around_followup(Some("")), None);
    }

    /// Scrolling up asks for the next older page only when the cache has none. The server's
    /// `before` cursor excludes that id, so before the oldest row is the page above this one,
    /// and before the anchor is the page on screen. A page of blank plaintext has to ask for
    /// the rows on screen even when older rows are already cached. A page of real words that
    /// already has older rows asks for nothing. The same id is asked once.
    #[test]
    fn a_blank_scrolled_page_asks_for_the_rows_on_screen() {
        assert_eq!(super::before_cursors("anchor", Some("oldest"), true, false), ["anchor"]);
        assert_eq!(super::before_cursors("anchor", Some("oldest"), false, true), ["oldest"]);
        assert_eq!(super::before_cursors("anchor", Some("oldest"), true, true), ["anchor", "oldest"]);
        assert!(super::before_cursors("anchor", Some("oldest"), false, false).is_empty());
        assert_eq!(super::before_cursors("same", Some("same"), true, true), ["same"]);
        assert_eq!(super::before_cursors("anchor", None, false, true), ["anchor"]);
        assert_eq!(super::before_cursors("anchor", Some(""), false, true), ["anchor"]);
    }

    #[test]
    fn a_repeated_people_search_is_already_done_and_one_waiting_is_left_alone() {
        assert!(matches!(
            super::search_fate(Some("zoe"), None, "zoe"),
            super::SearchFate::Repeated
        ));
        assert!(matches!(
            super::search_fate(Some("amy"), Some("zoe"), "zoe"),
            super::SearchFate::Waiting
        ));
        assert!(matches!(
            super::search_fate(None, None, "zoe"),
            super::SearchFate::Queue
        ));
        assert!(matches!(
            super::search_fate(Some("zoe"), Some("zoe"), "amy"),
            super::SearchFate::Queue
        ));
    }

    #[test]
    fn a_stored_account_stays_on_the_rail_while_its_session_is_unread() {
        assert_eq!(published_link(Link::SignedOut, [Link::Offline]), Link::Offline);
        assert_eq!(published_link(Link::SignedOut, std::iter::empty()), Link::SignedOut);
        assert_eq!(published_link(Link::Connected, [Link::Offline]), Link::Connected);
        assert_eq!(published_link(Link::Expired, [Link::Offline]), Link::Expired);
    }

    /// A signed-out history read must refuse. An empty Messages page is a successful newest
    /// answer, and the open pane clears itself with it even though the cache is still on disk.
    #[test]
    fn a_signed_out_history_read_is_a_refusal_not_an_empty_page() {
        assert_eq!(super::prove_signed_out_messages(), Ok(()));
        assert!(
            !matches!(super::hidden_history(), protocol::Response::Messages { .. }),
            "an empty page would clear the open pane"
        );
    }

    /// A signed-out Threads or Topics read must refuse. An empty Conversations page looks like
    /// a real empty list, and the home then draws none of the cached rows.
    #[test]
    fn a_signed_out_threads_read_is_a_refusal_not_an_empty_list() {
        assert_eq!(super::prove_signed_out_threads(), Ok(()));
        assert!(
            !matches!(super::hidden_list(), protocol::Response::Conversations { .. }),
            "an empty list would clear the drilled home"
        );
    }

    #[test]
    fn a_thread_id_names_its_parent_and_a_discord_name_is_one_line() {
        use protocol::{AccountKey, Key, Network};
        let key = |network, id: &str| Key::new(&AccountKey { network, account: "a".into() }, id);
        assert_eq!(
            super::derivable_thread(&key(Network::Slack, "C0:1.2")).as_ref().map(|(parent, root)| (parent.as_str(), root.as_str())),
            Some(("C0", "1.2"))
        );
        assert_eq!(
            super::derivable_thread(&key(Network::Teams, "19:c@thread.tacv2;messageid=9"))
                .as_ref()
                .map(|(parent, root)| (parent.as_str(), root.as_str())),
            Some(("19:c@thread.tacv2", "9"))
        );
        assert_eq!(super::derivable_thread(&key(Network::Slack, "C0:1.2:3")), None);
        assert_eq!(super::derivable_thread(&key(Network::Discord, "4101")), None);
        assert_eq!(super::thread_name("  Hello\nthere"), "Hello there");
        assert_eq!(super::thread_name("   "), "Thread");
        assert_eq!(super::thread_name(&"x".repeat(150)).chars().count(), 100);
        assert_eq!(super::first_line("Release train\nWho drives", 40), "Release train");
        assert!(super::first_line("A post with no subject that goes on for quite a while", 40).ends_with('…'));
    }

    #[test]
    fn a_text_sent_while_offline_waits_and_a_late_one_fails_instead_of_going() {
        assert_eq!(super::prove_offline_queue(), Ok(()));
    }

    #[test]
    fn filing_a_conversation_moves_it_to_archive_and_back() {
        use crate::adapters::{self, Command};
        use crate::hub::lock;
        use crate::store::{ConversationRow, Filing, Store};
        use protocol::{AccountKey, ConversationKind, Key, Link, Network, Request, Response, Shelf};
        use std::sync::Arc;

        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let chat = Arc::new(super::Chat::new(store));
        let account = AccountKey { network: Network::Telegram, account: "tg".into() };
        let conv = Key::new(&account, "u:42");
        let (adapter, ctx, rx) = adapters::local(account.clone(), Arc::clone(&chat.hub));
        lock(&chat.adapters).insert(account.clone(), adapter);
        chat.hub.set_link(&account, Link::Connected, "");
        ctx.conversation(&ConversationRow::new("u:42", ConversationKind::Direct, "Mara"))
        .unwrap();

        let worker_ctx = ctx.clone();
        let worker = std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::File { conversation_id, pinned, archived, folder_add, folder_remove, reply } => {
                        let row = worker_ctx.known(&conversation_id).unwrap().unwrap();
                        let mut folders = row.folders;
                        if let Some(f) = folder_add {
                            if !folders.contains(&f) {
                                folders.push(f);
                            }
                        }
                        if let Some(f) = folder_remove {
                            folders.retain(|x| x != &f);
                        }
                        let filing = Filing {
                            folders,
                            archived: archived.unwrap_or(row.archived),
                            pin: match pinned {
                                Some(true) => Some(0),
                                Some(false) => None,
                                None => row.pin,
                            },
                            hidden: row.hidden,
                            followed: row.followed,
                        };
                        worker_ctx.filing(&conversation_id, &filing).unwrap();
                        let _ = reply.send(Ok(()));
                    }
                    Command::Stop => break,
                    other => other.unsupported(&worker_ctx),
                }
            }
        });

        // Archive moves the chat from Main to Archive shelf.
        assert_eq!(
            chat.handle(Request::File {
                conversation: conv.clone(),
                pinned: None,
                archived: Some(true),
                folder_add: None,
                folder_remove: None,
            }),
            Response::Ok
        );
        let Response::Conversations { conversations: main_rows, .. } = chat.handle(Request::Home {
            network: Network::Telegram,
            account: Some(account.clone()),
            folder: None,
            shelf: Shelf::Main,
            offset: 0,
            limit: 50,
        }) else {
            panic!("expected Conversations");
        };
        assert!(main_rows.is_empty(), "archived chat must leave Main shelf");
        let Response::Conversations { conversations: arch_rows, .. } = chat.handle(Request::Home {
            network: Network::Telegram,
            account: Some(account.clone()),
            folder: None,
            shelf: Shelf::Archive,
            offset: 0,
            limit: 50,
        }) else {
            panic!("expected Conversations");
        };
        assert_eq!(arch_rows.len(), 1);
        assert!(arch_rows[0].archived);

        // Unarchive + pin + add to folder brings it back pinned and filed.
        assert_eq!(
            chat.handle(Request::File {
                conversation: conv.clone(),
                pinned: Some(true),
                archived: Some(false),
                folder_add: Some("work".into()),
                folder_remove: None,
            }),
            Response::Ok
        );
        let Response::Conversation { conversation: row } = chat.handle(Request::Conversation { conversation: conv }) else {
            panic!("expected Conversation");
        };
        assert!(!row.archived);
        assert_eq!(row.pin, Some(0));
        assert_eq!(row.folders, vec!["work".to_owned()]);

        lock(&chat.adapters).remove(&account);
        drop(ctx);
        let _ = worker.join();
    }

    #[test]
    fn delete_passes_scope_and_persists_conversation_rights() {
        use crate::adapters::{self, Command};
        use crate::hub::lock;
        use crate::store::{ConversationRow, Store};
        use protocol::{AccountKey, ConversationKind, ConversationRights, DeleteScope, Key, Link, Message, Network, Request, Response};
        use std::sync::{Arc, Mutex};

        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let chat = Arc::new(super::Chat::new(store));
        let account = AccountKey { network: Network::Telegram, account: "tg".into() };
        let conv = Key::new(&account, "g:1");
        let (adapter, ctx, rx) = adapters::local(account.clone(), Arc::clone(&chat.hub));
        lock(&chat.adapters).insert(account.clone(), adapter);
        chat.hub.set_link(&account, Link::Connected, "");
        let mut row = ConversationRow::new("g:1", ConversationKind::Group, "Mods");
        row.rights = ConversationRights { delete_others: true, ..ConversationRights::default() };
        ctx.conversation(&row).unwrap();
        let other_msg = Message::plain("m1", "other", "remove me", 1_700_000_000, false);
        let other_msg2 = Message::plain("m2", "other", "remove locally", 1_700_000_001, false);
        ctx.message("g:1", &other_msg).unwrap();
        ctx.message("g:1", &other_msg2).unwrap();

        let seen = Arc::new(Mutex::new(Vec::<(String, DeleteScope)>::new()));
        let seen_worker = Arc::clone(&seen);
        let worker_ctx = ctx.clone();
        let worker = std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::Delete { conversation_id, message_id, scope, reply } => {
                        seen_worker.lock().unwrap().push((message_id.clone(), scope));
                        worker_ctx.deleted(&conversation_id, &message_id).unwrap();
                        let _ = reply.send(Ok(()));
                    }
                    Command::Stop => break,
                    other => other.unsupported(&worker_ctx),
                }
            }
        });

        let Response::Conversation { conversation: stored } = chat.handle(Request::Conversation { conversation: conv.clone() }) else {
            panic!("expected Conversation");
        };
        assert!(stored.rights.delete_others);

        assert_eq!(
            chat.handle(Request::Delete { conversation: conv.clone(), message_id: "m1".into(), scope: DeleteScope::Everyone }),
            Response::Ok
        );
        assert_eq!(
            chat.handle(Request::Delete { conversation: conv, message_id: "m2".into(), scope: DeleteScope::Me }),
            Response::Ok
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("m1".to_owned(), DeleteScope::Everyone), ("m2".to_owned(), DeleteScope::Me)]
        );

        lock(&chat.adapters).remove(&account);
        drop(ctx);
        let _ = worker.join();
    }

    #[test]
    fn disappearing_timer_updates_row_and_info() {
        use crate::adapters::{self, Command, ConversationAbout};
        use crate::hub::lock;
        use crate::store::{ConversationRow, Store};
        use protocol::{AccountKey, ConversationKind, Key, Link, Network, Request, Response};
        use std::sync::Arc;

        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let chat = Arc::new(super::Chat::new(store));
        let account = AccountKey { network: Network::Signal, account: "sig".into() };
        let conv = Key::new(&account, "u:9");
        let (adapter, ctx, rx) = adapters::local(account.clone(), Arc::clone(&chat.hub));
        lock(&chat.adapters).insert(account.clone(), adapter);
        chat.hub.set_link(&account, Link::Connected, "");
        ctx.conversation(&ConversationRow::new("u:9", ConversationKind::Direct, "Nora")).unwrap();

        let worker_ctx = ctx.clone();
        let worker = std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::SetDisappearing { conversation_id, seconds, reply } => {
                        worker_ctx.disappearing(&conversation_id, (seconds > 0).then_some(seconds)).unwrap();
                        let _ = reply.send(Ok(()));
                    }
                    Command::ConversationInfo { reply, .. } => {
                        let _ = reply.send(Ok(ConversationAbout::default()));
                    }
                    Command::Stop => break,
                    other => other.unsupported(&worker_ctx),
                }
            }
        });

        assert_eq!(chat.handle(Request::SetDisappearing { conversation: conv.clone(), seconds: 30 }), Response::Ok);
        let Response::Conversation { conversation: row } = chat.handle(Request::Conversation { conversation: conv.clone() }) else {
            panic!("expected Conversation");
        };
        assert_eq!(row.disappearing, Some(30));

        let Response::ConversationInfo { info } = chat.handle(Request::ConversationInfo { conversation: conv.clone() }) else {
            panic!("expected ConversationInfo");
        };
        assert_eq!(info.disappearing, Some(30));

        assert_eq!(chat.handle(Request::SetDisappearing { conversation: conv.clone(), seconds: 0 }), Response::Ok);
        let Response::Conversation { conversation: cleared } = chat.handle(Request::Conversation { conversation: conv }) else {
            panic!("expected Conversation");
        };
        assert_eq!(cleared.disappearing, None);

        lock(&chat.adapters).remove(&account);
        drop(ctx);
        let _ = worker.join();
    }

    #[test]
    fn conversation_management_requests_update_conversation_and_members() {
        use crate::adapters::{self, Command};
        use crate::hub::lock;
        use crate::store::{ConversationRow, Store};
        use protocol::{AccountKey, ConversationKind, ConversationRights, ErrorCode, Key, Link, Network, Request, Response};
        use std::sync::{Arc, Mutex};

        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let chat = Arc::new(super::Chat::new(store));
        let account = AccountKey { network: Network::Slack, account: "w1".into() };
        let conv = Key::new(&account, "C1");
        let (adapter, ctx, rx) = adapters::local(account.clone(), Arc::clone(&chat.hub));
        lock(&chat.adapters).insert(account.clone(), adapter);
        chat.hub.set_link(&account, Link::Connected, "");
        let mut row = ConversationRow::new("C1", ConversationKind::Group, "Old Name");
        row.rights = ConversationRights {
            add_members: true,
            remove_members: true,
            rename: true,
            edit_topic: true,
            edit_picture: true,
            ..ConversationRights::default()
        };
        ctx.conversation(&row).unwrap();

        let members = Arc::new(Mutex::new(vec!["U1".to_owned()]));
        let members_worker = Arc::clone(&members);
        let worker_ctx = ctx.clone();
        let worker = std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::MembersAdd { people, reply, .. } => {
                        members_worker.lock().unwrap().extend(people);
                        let _ = reply.send(Ok(()));
                    }
                    Command::MembersRemove { person, reply, .. } => {
                        members_worker.lock().unwrap().retain(|p| p != &person);
                        let _ = reply.send(Ok(()));
                    }
                    Command::ConversationUpdate { conversation_id, title, topic, description, reply } => {
                        let cur = worker_ctx.known(&conversation_id).unwrap().unwrap();
                        let mut next = ConversationRow::new(conversation_id, cur.kind, title.unwrap_or(cur.title));
                        next.topic = topic.or(description).unwrap_or(cur.topic);
                        next.rights = cur.rights;
                        worker_ctx.conversation(&next).unwrap();
                        let _ = reply.send(Ok(()));
                    }
                    Command::ConversationPicture { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    Command::Stop => break,
                    other => other.unsupported(&worker_ctx),
                }
            }
        });

        // Empty people list and empty title are refused cleanly.
        assert!(matches!(
            chat.handle(Request::MembersAdd { conversation: conv.clone(), people: vec![] }),
            Response::Error { code: ErrorCode::Refused, .. }
        ));
        assert!(matches!(
            chat.handle(Request::ConversationUpdate {
                conversation: conv.clone(),
                title: Some("   ".into()),
                topic: None,
                description: None,
            }),
            Response::Error { code: ErrorCode::Refused, .. }
        ));

        // Add, remove, rename + topic, and picture succeed.
        assert_eq!(chat.handle(Request::MembersAdd { conversation: conv.clone(), people: vec!["U2".into()] }), Response::Ok);
        assert_eq!(*members.lock().unwrap(), vec!["U1".to_owned(), "U2".to_owned()]);
        assert_eq!(chat.handle(Request::MembersRemove { conversation: conv.clone(), person: "U2".into() }), Response::Ok);
        assert_eq!(*members.lock().unwrap(), vec!["U1".to_owned()]);
        assert_eq!(
            chat.handle(Request::ConversationUpdate {
                conversation: conv.clone(),
                title: Some("New Name".into()),
                topic: Some("Ship week".into()),
                description: None,
            }),
            Response::Ok
        );
        let Response::Conversation { conversation: updated } = chat.handle(Request::Conversation { conversation: conv.clone() }) else {
            panic!("expected Conversation");
        };
        assert_eq!(updated.title, "New Name");
        assert_eq!(updated.topic, "Ship week");
        assert!(updated.rights.add_members && updated.rights.remove_members && updated.rights.rename && updated.rights.edit_topic && updated.rights.edit_picture);
        assert_eq!(chat.handle(Request::ConversationPicture { conversation: conv, path: "/tmp/pic.png".into() }), Response::Ok);

        lock(&chat.adapters).remove(&account);
        drop(ctx);
        let _ = worker.join();
    }

    #[test]
    fn tiny_channel_mark_read_sends_command_with_receipts_off_but_direct_does_not() {
        use super::*;
        let store = Store::open(std::path::Path::new(":memory:")).expect("memory store");
        let chat = Chat::new(store);
        let hub = chat.hub.clone();
        let account = AccountKey { network: Network::Tiny, account: "me".into() };
        hub.set_setting(&protocol::settings::account_scope(&account), protocol::settings::RECEIPTS, Some("off".into())).unwrap();
        assert!(!hub.settings().receipts(&account));

        let (adapter, ctx, inbox) = crate::adapters::local(account.clone(), hub);
        lock(&chat.adapters).insert(account.clone(), adapter);

        let mut ch_row = ConversationRow::new("ch-1", ConversationKind::Channel, "general");
        ch_row.space = Some("server".into());
        ctx.conversation(&ch_row).unwrap();
        ctx.counts("ch-1", 2, 0, None, 10).unwrap();

        let mut dm_row = ConversationRow::new("dm-1", ConversationKind::Direct, "Bea");
        dm_row.space = Some("server".into());
        ctx.conversation(&dm_row).unwrap();
        ctx.counts("dm-1", 1, 0, None, 10).unwrap();

        let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let seen_worker = seen.clone();
        let worker = std::thread::spawn(move || {
            for cmd in inbox {
                match cmd {
                    Command::MarkRead { conversation_id, message_id } => {
                        seen_worker.lock().unwrap().push((conversation_id, message_id));
                    }
                    Command::Stop => break,
                    _ => {}
                }
            }
        });

        assert_eq!(
            chat.handle(Request::MarkRead { conversation: Key::new(&account, "ch-1"), message_id: "m-1".into() }),
            Response::Ok
        );
        assert_eq!(
            chat.handle(Request::MarkRead { conversation: Key::new(&account, "dm-1"), message_id: "m-2".into() }),
            Response::Ok
        );

        lock(&chat.adapters).remove(&account);
        drop(ctx);
        let _ = worker.join();

        let commands = seen.lock().unwrap().clone();
        assert_eq!(commands, vec![("ch-1".to_owned(), "m-1".to_owned())]);
    }
}
