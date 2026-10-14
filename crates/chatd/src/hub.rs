use crate::media::Calls;
use crate::store::Store;
use protocol::settings::Values;
use protocol::{
    clip, AccountKey, Activity, Capabilities, Change, Key, Keychain, Link, Network, Occupants,
    Presence, Typist, Watching, OCCUPANTS_LIMIT,
};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Changes remembered for a shell that is behind. One entry per thing that changed, so this is
/// also the most a `Wait` answers with.
const CHANGES: usize = 256;
/// Presence entries held: friends, direct partners and one visible member window.
const PRESENCES: usize = 2_000;
/// Voice channels whose occupants are held. A channel past this is simply not shown as occupied.
const OCCUPIED: usize = 256;
/// The shell's `Watch` lapses when it has not asked `Wait` for this long.
const WATCH_TTL: Duration = Duration::from_secs(90);
/// A keychain read that takes longer than this is macOS asking the user.
pub const KEYCHAIN_PROMPT: Duration = Duration::from_secs(1);
/// Upload progress is told at most this often per message.
const UPLOAD_EVERY: Duration = Duration::from_millis(250);
/// Rows whose preview was asked for, remembered so none is asked twice; cleared past this.
const PREVIEWS_ASKED: usize = 4_000;
/// How long a profile, an info pane or a catalog page asked of a network is answered from here.
pub const ANSWER_TTL: Duration = Duration::from_secs(300);

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A lock that a panic elsewhere left poisoned is still the data it guards: every write here is
/// whole before the guard drops, and the store's transactions roll back on unwind. Taking it
/// anyway keeps one panicking request from failing every later one.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A catalog page as asked: account, kind, Space, query, cursor.
pub type CatalogAsked = (AccountKey, protocol::CatalogKind, Option<Key>, String, Option<String>);

/// Answers asked of a network, kept a few minutes so a card opened twice asks once. Bounded:
/// the oldest goes when it is full.
pub struct Recent<K, V> {
    held: Mutex<Vec<(K, Instant, V)>>,
    limit: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> Recent<K, V> {
    pub fn new(limit: usize) -> Recent<K, V> {
        Recent { held: Mutex::new(Vec::new()), limit }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let mut held = lock(&self.held);
        held.retain(|(_, at, _)| at.elapsed() < ANSWER_TTL);
        held.iter().find(|(held, _, _)| held == key).map(|(_, _, value)| value.clone())
    }

    pub fn keep(&self, key: K, value: V) {
        let mut held = lock(&self.held);
        held.retain(|(held, at, _)| *held != key && at.elapsed() < ANSWER_TTL);
        if held.len() >= self.limit {
            held.remove(0);
        }
        held.push((key, Instant::now(), value));
    }

    /// Forgets what was kept for one key, after something changed it (a mute set).
    pub fn forget(&self, key: &K) {
        lock(&self.held).retain(|(held, _, _)| held != key);
    }
}

/// What changed, newest last. `seq` goes up by one per change; an entry is replaced when the same
/// thing changes again, so the list is bounded by how many things there are, not by how busy.
struct Changes {
    seq: u64,
    /// A shell that last heard an older `seq` than this may have missed something.
    floor: u64,
    entries: Vec<(u64, Change)>,
}

struct Shown {
    presence: Presence,
    status: String,
    emoji: String,
    /// Boxed: thousands of people are held, and few are doing anything.
    activity: Option<Box<Activity>>,
    /// On a list the user owns, so kept whether or not the shell is watching.
    owned: bool,
}

/// Presence and what goes with it, as the shell is told it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doing {
    pub presence: Presence,
    pub status: String,
    pub emoji: String,
    pub activity: Option<Activity>,
}

/// One account that has a session, and how its connection is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub account: AccountKey,
    pub link: Link,
    /// The network's reason for the link, in words.
    pub detail: String,
    /// `Offline`, and trying again within seconds.
    pub reconnecting: bool,
    /// What the user must do in the network's own app before this account works. Empty: nothing.
    pub required: String,
    /// What it may send, as the adapter last said.
    pub can: Capabilities,
}

/// The session reads at start, and whatever is waiting on the keychain.
#[derive(Default)]
struct Reading {
    /// Networks not read yet.
    pending: Vec<Network>,
    /// The network being read now, and since when.
    now: Option<(Network, Instant)>,
    denied: Vec<Network>,
}

/// What is known only while it is true: never stored, bounded, gone at restart.
struct Live {
    typing: HashMap<Key, Vec<Typist>>,
    occupants: HashMap<Key, Occupants>,
    presence: HashMap<Key, Shown>,
    watching: Arc<Watching>,
    asked: Instant,
    /// Uploads, by message, and when their progress was last told.
    uploads: HashMap<(Key, String), Instant>,
    /// Rows whose preview was asked of their network.
    previews: HashSet<Key>,
}

/// What the socket server and the adapters share: the store, each account's link, what changed,
/// and the live state that is never stored.
pub struct Hub {
    store: Mutex<Store>,
    links: Mutex<Vec<Linked>>,
    changes: Mutex<Changes>,
    moved: Condvar,
    live: Mutex<Live>,
    reading: Mutex<Reading>,
    /// When this chatd started, unix milliseconds: `Status.started`.
    pub started: u64,
    pub profiles: Recent<(Key, Option<Key>), protocol::Profile>,
    pub infos: Recent<Key, protocol::ConversationInfo>,
    pub space_infos: Recent<Key, protocol::SpaceInfo>,
    pub catalogs: Recent<CatalogAsked, (Vec<protocol::CatalogItem>, Option<String>)>,
    pub calls: Calls,
    /// The settings as stored (`protocol::settings`), read once and kept in step with every
    /// `Set`, so a typing signal or a call start asks no query.
    settings: Mutex<Values>,
    /// Retention runs when this is set, and once a day.
    prune_now: Mutex<bool>,
    prune_wake: Condvar,
}

impl Hub {
    pub fn new(store: Store) -> Hub {
        let settings = Values::new(store.settings().unwrap_or_default());
        // Starting from the clock makes a `seq` from an earlier run older than `floor`, so a
        // shell that outlived a restart is told to read everything again.
        let start = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_micros() as u64);
        Hub {
            store: Mutex::new(store),
            links: Mutex::new(Vec::new()),
            changes: Mutex::new(Changes {
                seq: start,
                floor: start,
                entries: Vec::new(),
            }),
            moved: Condvar::new(),
            live: Mutex::new(Live {
                typing: HashMap::new(),
                occupants: HashMap::new(),
                presence: HashMap::new(),
                watching: Arc::default(),
                asked: Instant::now(),
                uploads: HashMap::new(),
                previews: HashSet::new(),
            }),
            reading: Mutex::default(),
            started: start / 1_000,
            profiles: Recent::new(64),
            infos: Recent::new(64),
            space_infos: Recent::new(16),
            catalogs: Recent::new(32),
            calls: Calls::default(),
            settings: Mutex::new(settings),
            prune_now: Mutex::new(false),
            prune_wake: Condvar::new(),
        }
    }

    // Settings.

    pub fn settings(&self) -> MutexGuard<'_, Values> {
        lock(&self.settings)
    }

    /// Stores one setting and keeps the copy here in step. Tells every window.
    pub fn set_setting(&self, scope: &str, key: &str, value: Option<String>) -> rusqlite::Result<()> {
        // The store first: the copy here never holds what was not saved.
        let mut settings = lock(&self.settings);
        self.store().set_setting(scope, key, value.as_deref())?;
        settings.set(scope, key, value);
        drop(settings);
        self.changed(Change::Settings);
        Ok(())
    }

    /// Reads the settings again after the store removed rows itself (sign-out).
    pub fn reload_settings(&self) {
        let mut settings = lock(&self.settings);
        if let Ok(stored) = self.store().settings() {
            *settings = Values::new(stored);
        }
        drop(settings);
        self.changed(Change::Settings);
    }

    /// Reset all: every row but the version goes.
    pub fn reset_settings(&self) -> rusqlite::Result<()> {
        let mut settings = lock(&self.settings);
        let mut store = self.store();
        store.reset_settings()?;
        *settings = Values::new(store.settings()?);
        drop((store, settings));
        self.changed(Change::Settings);
        Ok(())
    }

    /// Has retention run now rather than at its daily time.
    pub fn prune_soon(&self) {
        *lock(&self.prune_now) = true;
        self.prune_wake.notify_all();
    }

    /// Waits until retention is due: `every`, or sooner when asked.
    pub fn prune_due(&self, every: Duration) {
        let asked = lock(&self.prune_now);
        let (mut asked, _) = self.prune_wake.wait_timeout_while(asked, every, |asked| !*asked).unwrap_or_else(PoisonError::into_inner);
        *asked = false;
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        lock(&self.store)
    }

    // The keychain.

    /// These networks' sessions are about to be read, one at a time.
    pub fn reading(&self, networks: &[Network]) {
        let mut reading = lock(&self.reading);
        reading.denied.retain(|network| !networks.contains(network));
        for network in networks {
            if !reading.pending.contains(network) {
                reading.pending.push(*network);
            }
        }
        drop(reading);
        self.changed(Change::Status);
    }

    /// The read of one network's session began (`Some`), or ended, denied or not.
    pub fn read_now(&self, network: Network, began: bool, denied: bool) {
        let mut reading = lock(&self.reading);
        if began {
            reading.now = Some((network, Instant::now()));
            return;
        }
        reading.now = None;
        reading.pending.retain(|pending| *pending != network);
        if denied && !reading.denied.contains(&network) {
            reading.denied.push(network);
        }
        drop(reading);
        self.changed(Change::Status);
    }

    /// The read under way has been waiting long enough to be a prompt on screen.
    pub fn read_waiting(&self) {
        self.changed(Change::Status);
    }

    /// Networks whose session has not been read yet.
    pub fn sessions_pending(&self) -> Vec<Network> {
        lock(&self.reading).pending.clone()
    }

    pub fn keychain(&self) -> Keychain {
        let reading = lock(&self.reading);
        Keychain {
            reading: !reading.pending.is_empty(),
            waiting: reading.now.filter(|(_, since)| since.elapsed() >= KEYCHAIN_PROMPT).map(|(network, _)| network),
            denied: reading.denied.clone(),
        }
    }

    // Links.

    pub fn link(&self, account: &AccountKey) -> Link {
        let links = lock(&self.links);
        links.iter().find(|linked| linked.account == *account).map_or(Link::SignedOut, |linked| linked.link)
    }

    /// Every account that has a session, with its link and the network's reason for it.
    pub fn links(&self) -> Vec<Linked> {
        lock(&self.links).clone()
    }

    /// The best link among a network's accounts.
    pub fn network_link(&self, network: Network) -> Link {
        let rank = |link: Link| match link {
            Link::SignedOut => 0,
            Link::Expired => 1,
            Link::Offline => 2,
            Link::Connected => 3,
        };
        let links = lock(&self.links);
        let of_network = links.iter().filter(|linked| linked.account.network == network);
        of_network.map(|linked| linked.link).max_by_key(|link| rank(*link)).unwrap_or(Link::SignedOut)
    }

    /// `SignedOut` removes the account from the list.
    pub fn set_link(&self, account: &AccountKey, link: Link, detail: &str) {
        self.set_linked(account, link, detail, false);
    }

    /// The connection dropped. `retrying` says the next attempt is seconds away, which the shell
    /// words differently from an account that is simply down.
    pub fn set_offline(&self, account: &AccountKey, retrying: bool) {
        self.set_linked(account, Link::Offline, "", retrying);
    }

    fn set_linked(&self, account: &AccountKey, link: Link, detail: &str, reconnecting: bool) {
        {
            let mut links = lock(&self.links);
            let at = links.iter().position(|linked| linked.account == *account);
            match (at, link) {
                (Some(at), Link::SignedOut) => {
                    links.remove(at);
                }
                (None, Link::SignedOut) => return,
                (Some(at), _) => {
                    let linked = &mut links[at];
                    if (linked.link, linked.detail.as_str(), linked.reconnecting) == (link, detail, reconnecting) {
                        return;
                    }
                    linked.link = link;
                    linked.detail = detail.to_owned();
                    linked.reconnecting = reconnecting;
                }
                (None, _) => links.push(Linked {
                    account: account.clone(),
                    link,
                    detail: detail.to_owned(),
                    reconnecting,
                    required: String::new(),
                    can: Capabilities::default(),
                }),
            }
        }
        self.changed(Change::Status);
    }

    /// What the user must do in the network's own app (a captcha, a verification), in words,
    /// or empty once it is done. Kept beside the link, whatever the link is.
    pub fn set_required(&self, account: &AccountKey, required: &str) {
        {
            let mut links = lock(&self.links);
            let Some(linked) = links.iter_mut().find(|linked| linked.account == *account) else {
                return;
            };
            if linked.required == required {
                return;
            }
            linked.required = required.to_owned();
            clip(&mut linked.required, 300);
        }
        self.changed(Change::Status);
    }

    /// What an account may send, as its adapter says.
    pub fn set_capabilities(&self, account: &AccountKey, can: Capabilities) {
        {
            let mut links = lock(&self.links);
            let Some(linked) = links.iter_mut().find(|linked| linked.account == *account) else {
                return;
            };
            if linked.can == can {
                return;
            }
            linked.can = can;
        }
        self.changed(Change::Status);
    }

    pub fn capabilities(&self, account: &AccountKey) -> Capabilities {
        lock(&self.links).iter().find(|linked| linked.account == *account).map_or_else(Capabilities::default, |linked| linked.can)
    }

    // Changes.

    /// Call after anything the shell draws has changed, naming what.
    pub fn changed(&self, change: Change) {
        let mut changes = lock(&self.changes);
        changes.seq += 1;
        let seq = changes.seq;
        changes.entries.retain(|(_, old)| !old.same_scope(&change));
        changes.entries.push((seq, change));
        if changes.entries.len() > CHANGES {
            let (dropped, _) = changes.entries.remove(0);
            changes.floor = dropped;
        }
        drop(changes);
        self.moved.notify_all();
    }

    /// Everything a shell holds may be stale (the cache was cleared): its next `Wait` answers
    /// `reset`, and it reads what is on screen again.
    pub fn reset(&self) {
        let mut changes = lock(&self.changes);
        changes.seq += 1;
        changes.floor = changes.seq;
        changes.entries.clear();
        drop(changes);
        self.moved.notify_all();
    }

    pub fn seq(&self) -> u64 {
        lock(&self.changes).seq
    }

    /// Blocks until something has changed since `since`, or `limit` passes. Answers the new `seq`,
    /// whether the caller must read everything again, and what changed.
    pub fn wait(&self, since: u64, limit: Duration) -> (u64, bool, Vec<Change>) {
        lock(&self.live).asked = Instant::now();
        let changes = lock(&self.changes);
        let (changes, _) = self
            .moved
            .wait_timeout_while(changes, limit, |changes| changes.seq == since)
            .unwrap_or_else(PoisonError::into_inner);
        if since < changes.floor || since > changes.seq {
            return (changes.seq, true, Vec::new());
        }
        let newer = changes.entries.iter().filter(|(seq, _)| *seq > since);
        (changes.seq, false, newer.map(|(_, change)| change.clone()).collect())
    }

    // What the shell has on screen.

    /// Replaces what the shell watches. True if it is different from before.
    pub fn watch(&self, watching: Watching) -> bool {
        let mut live = lock(&self.live);
        live.asked = Instant::now();
        if *live.watching == watching {
            return false;
        }
        let Live { presence, watching: held, .. } = &mut *live;
        presence.retain(|person, shown| shown.owned || watching.people.contains(person));
        *held = Arc::new(watching);
        true
    }

    /// What the shell has on screen, or nothing when it has gone away without saying so.
    pub fn watching(&self) -> Arc<Watching> {
        let mut live = lock(&self.live);
        if live.asked.elapsed() > WATCH_TTL && *live.watching != Watching::default() {
            live.watching = Arc::default();
            live.presence.retain(|_, shown| shown.owned);
        }
        live.watching.clone()
    }

    // Presence and typing.

    /// Kept only for people on a list the user owns and people the shell is watching; anything
    /// else is dropped here, which is what bounds presence whatever the network sends.
    pub fn set_presence(&self, person: Key, presence: Presence, status: &str, owned: bool) {
        self.set_shown(person, owned, |shown| {
            shown.presence = presence;
            shown.status = status.to_owned();
        });
    }

    /// Their status emoji and what they are doing, beside their presence. Kept as presence is.
    pub fn set_activity(&self, person: Key, emoji: &str, activity: Option<Activity>, owned: bool) {
        let activity = activity.map(|mut activity| {
            activity.bound();
            activity
        });
        self.set_shown(person, owned, |shown| {
            shown.emoji = emoji.chars().take(16).collect();
            shown.activity = activity;
        });
    }

    fn set_shown(&self, person: Key, owned: bool, change: impl FnOnce(&mut Doing)) {
        let watched = owned || self.watching().people.contains(&person);
        let doing = {
            let mut live = lock(&self.live);
            let known = live.presence.get(&person);
            if !watched || (known.is_none() && live.presence.len() >= PRESENCES) {
                return;
            }
            let before = known.map_or_else(Doing::default, |known| Doing {
                presence: known.presence,
                status: known.status.clone(),
                emoji: known.emoji.clone(),
                activity: known.activity.as_deref().cloned(),
            });
            let mut doing = before.clone();
            change(&mut doing);
            if known.is_some() && doing == before {
                return;
            }
            let shown = Shown {
                presence: doing.presence,
                status: doing.status.clone(),
                emoji: doing.emoji.clone(),
                activity: doing.activity.clone().map(Box::new),
                owned: owned || known.is_some_and(|known| known.owned),
            };
            live.presence.insert(person.clone(), shown);
            doing
        };
        self.changed(Change::Presence {
            person,
            presence: doing.presence,
            status: doing.status,
            emoji: doing.emoji,
            activity: doing.activity,
        });
    }

    pub fn presence(&self, person: &Key) -> Presence {
        let live = lock(&self.live);
        live.presence.get(person).map_or(Presence::Unknown, |shown| shown.presence)
    }

    /// The same, with the status text that came with it.
    pub fn shown(&self, person: &Key) -> (Presence, String) {
        let live = lock(&self.live);
        live.presence.get(person).map_or((Presence::Unknown, String::new()), |shown| (shown.presence, shown.status.clone()))
    }

    /// Everything held about what they are doing now.
    pub fn doing(&self, person: &Key) -> Doing {
        let live = lock(&self.live);
        live.presence.get(person).map_or_else(Doing::default, |shown| Doing {
            presence: shown.presence,
            status: shown.status.clone(),
            emoji: shown.emoji.clone(),
            activity: shown.activity.as_deref().cloned(),
        })
    }

    // Uploads and previews.

    /// How far a pending message's files have gone. Told at most four times a second, and
    /// always at the end.
    pub fn set_upload(&self, conversation: Key, client_id: &str, sent: u64, total: u64) {
        {
            let mut live = lock(&self.live);
            let id = (conversation.clone(), client_id.to_owned());
            let done = sent >= total;
            if !done && live.uploads.get(&id).is_some_and(|at| at.elapsed() < UPLOAD_EVERY) {
                return;
            }
            if done {
                live.uploads.remove(&id);
            } else {
                live.uploads.retain(|_, at| at.elapsed() < Duration::from_secs(600));
                live.uploads.insert(id, Instant::now());
            }
        }
        self.changed(Change::Upload { conversation, client_id: client_id.to_owned(), sent, total });
    }

    /// Of these rows, the ones whose preview has not been asked of the network before. They
    /// count as asked from now on.
    pub fn ask_previews(&self, rows: Vec<Key>) -> Vec<Key> {
        let mut live = lock(&self.live);
        if live.previews.len() > PREVIEWS_ASKED {
            live.previews.clear();
        }
        rows.into_iter().filter(|row| live.previews.insert(row.clone())).collect()
    }

    /// Voice channels with someone in them now, and who.
    pub fn occupied(&self) -> Vec<(Key, Occupants)> {
        let live = lock(&self.live);
        live.occupants.iter().map(|(conversation, occupants)| (conversation.clone(), occupants.clone())).collect()
    }

    // Voice channels.

    /// Who is in a voice channel now, as the network lists them. Kept for at most [`OCCUPIED`]
    /// channels and [`OCCUPANTS_LIMIT`] people each; an empty channel is forgotten.
    pub fn set_occupants(&self, conversation: Key, mut occupants: Occupants) {
        occupants.people.truncate(OCCUPANTS_LIMIT);
        for occupant in &mut occupants.people {
            clip(&mut occupant.name, 80);
        }
        occupants.total = occupants.total.max(occupants.people.len() as u32);
        {
            let mut live = lock(&self.live);
            let known = live.occupants.get(&conversation);
            let empty = occupants.total == 0;
            if known == Some(&occupants) || (known.is_none() && (empty || live.occupants.len() >= OCCUPIED)) {
                return;
            }
            if empty {
                live.occupants.remove(&conversation);
            } else {
                live.occupants.insert(conversation.clone(), occupants.clone());
            }
        }
        self.changed(Change::Occupants { conversation, occupants });
    }

    pub fn occupants(&self, conversation: &Key) -> Option<Occupants> {
        lock(&self.live).occupants.get(conversation).cloned()
    }

    /// Someone is typing until `typist.until`, or stopped when `typing` is false. Lines past their
    /// time are dropped here whenever the conversation is touched; the shell times them out on
    /// screen itself.
    pub fn set_typing(&self, conversation: Key, typist: Typist, typing: bool) {
        let typists = {
            let mut live = lock(&self.live);
            let now = now();
            let typists = live.typing.entry(conversation.clone()).or_default();
            let before = typists.len();
            typists.retain(|other| other.id != typist.id && other.until > now);
            if !typing && typists.len() == before {
                return;
            }
            if typing {
                typists.push(typist);
            }
            let typists = typists.clone();
            live.typing.retain(|_, typists| !typists.is_empty());
            typists
        };
        self.changed(Change::Typing { conversation, typists });
    }

    pub fn typists(&self, conversation: &Key) -> Vec<Typist> {
        let live = lock(&self.live);
        let now = now();
        let typists = live.typing.get(conversation).into_iter().flatten();
        typists.filter(|typist| typist.until > now).cloned().collect()
    }

    /// Drops what is known live about an account, for sign-out and for a dropped socket. A
    /// voice channel that was shown as occupied is told to the shell as empty: nothing else
    /// would ever take the people off its row.
    pub fn forget_live(&self, account: &AccountKey) {
        let emptied: Vec<Key> = {
            let mut live = lock(&self.live);
            live.presence.retain(|person, _| person.account() != *account);
            live.typing.retain(|conversation, _| conversation.account() != *account);
            let emptied = live.occupants.keys().filter(|conversation| conversation.account() == *account).cloned().collect();
            live.occupants.retain(|conversation, _| conversation.account() != *account);
            emptied
        };
        for conversation in emptied {
            self.changed(Change::Occupants { conversation, occupants: Occupants::default() });
        }
    }
}
