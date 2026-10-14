//! The media cache: files chatd fetches for the shell. Bytes go from the network to disk and
//! nowhere else; chatd never decodes them and the socket only ever carries paths.
//!
//! Rules (`docs/product/07-budgets-and-risk.md`):
//! - Files live under `<support dir>/media/<network>/<account>/`, named by a hash of the media
//!   id. A changed picture is a new id, so nothing is ever revalidated and the old one ages out.
//! - At most [`IN_FLIGHT`] fetches at once, the newest request first: what is on screen now
//!   matters more than what scrolled past. A fetch is started only by `request`, which the
//!   shell sends for what is on screen; nothing is fetched ahead.
//! - Only the address the adapter registered is fetched (`Store::register_media`), and only
//!   from the network's own hosts (`fetch::Hosts`). The adapter's `Command::Download` supplies
//!   the session's headers or a fresh signed address. Never an arbitrary link from a message.
//! - A file is streamed to a part file through one buffer and renamed when whole. Its size is
//!   capped by kind, from the headers and again while it arrives.
//! - [`CACHE_BYTES`] on disk in all; least recently used goes first (`Store::media_on_disk`),
//!   except what was asked for in the last minute.
//! - A fetch that fails is recorded with its reason (`Store::media_failed`) and announced once.
//!   The file is not fetched again until `Fetch` says `retry` or the adapter registers a fresh
//!   address, however often the shell asks: an expired address must not become a request to
//!   the network on every frame.
//! - The size the shell draws at is the store's to choose (`Store::media_sized`, before
//!   `request`): the cache fetches whatever address the row names now, and a file whose
//!   address changed while it was arriving is fetched again rather than kept.
//! - Sign-out deletes the account's directory.
//!
//! The shell asks for a thumbnail and an original by their own ids (`Attachment::thumb`,
//! `Attachment::file`), so the original is fetched only when it is asked for.

mod fetch;

use crate::adapters::Command;
use crate::hub::{now, Hub};
use crate::session;
use protocol::{AccountKey, Change, MediaFile, Network};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const CACHE_BYTES: u64 = 500 * 1024 * 1024;
pub const IN_FLIGHT: usize = 2;
/// Most fetches waiting their turn. Past this the oldest request gives way: it is the one
/// most likely to have scrolled off screen, and the shell asks again if it is still there.
const WAITING: usize = 256;
/// A file asked for this recently is on screen, so it is never evicted.
const ON_SCREEN: Duration = Duration::from_secs(60);
/// Most files whose expired addresses are renewed in one request to the network, the one
/// being fetched included (Discord's own limit for `attachments/refresh-urls`).
const RENEWED_TOGETHER: usize = 50;
/// Most files whose last place on disk is remembered, to remove a smaller size once a larger
/// one lands. What this forgets, `reconcile` removes at the next start.
const REMEMBERED: usize = 4_096;

/// Where an account's files go under the `media` directory.
pub fn dir_under(media: &Path, account: &AccountKey) -> PathBuf {
    media.join(account.network.label().to_lowercase()).join(&account.account)
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3))
}

/// A file's name on disk: FNV-1a of its id, so any id is a safe name of one length.
fn name_of(id: &str) -> String {
    format!("{:016x}", fnv(id))
}

/// Where a file is kept once it is on disk.
pub fn path_of(media: &Path, account: &AccountKey, id: &str) -> PathBuf {
    dir_under(media, account).join(name_of(id))
}

/// Where the cache keeps what it fetched from `url`. Another size of the same picture is
/// another address and so another path: a shell that holds the smaller one decoded can tell
/// that what landed is new.
fn path_from(media: &Path, account: &AccountKey, id: &str, url: &str) -> PathBuf {
    dir_under(media, account).join(format!("{}-{:08x}", name_of(id), fnv(url) as u32))
}

type FileKey = (AccountKey, String);

struct Job {
    file: FileKey,
    /// The account's sign-out count when this was asked for.
    epoch: u64,
    adapter: Option<Sender<Command>>,
}

#[derive(Default)]
struct Queue {
    /// False until the directory has been checked against the store at start.
    ready: bool,
    /// Newest last. Workers take from the end.
    waiting: Vec<Job>,
    fetching: Vec<FileKey>,
    asked: HashMap<FileKey, Instant>,
    /// Where each file fetched in this run was put.
    landed: HashMap<FileKey, PathBuf>,
    /// How many times each account has been signed out since start.
    epochs: HashMap<AccountKey, u64>,
}

struct Cache {
    hub: Arc<Hub>,
    dir: PathBuf,
    /// Bytes on disk in all. `TINYCHAT_MEDIA_CAP` replaces it in a scratch directory.
    cap: u64,
    /// `TINYCHAT_MEDIA_ON_SCREEN`, in seconds, replaces [`ON_SCREEN`] in a scratch directory.
    on_screen: Duration,
    hosts: fetch::Hosts,
    queue: Mutex<Queue>,
    wake: Condvar,
    evicting: Mutex<()>,
}

pub struct Blobs {
    cache: Arc<Cache>,
}

/// A test's override. Read only in a scratch directory, like the sessions in `session::load`.
fn scratch(name: &str) -> Option<String> {
    if session::enabled() {
        return None;
    }
    std::env::var(name).ok()
}

impl Blobs {
    /// `dir` is the `media` directory under the support directory.
    pub fn new(hub: Arc<Hub>, dir: PathBuf) -> Blobs {
        // `TINYCHAT_MEDIA_HOSTS`: hosts a test serves files from, separated by commas.
        let mut local: Vec<(String, fetch::Trust)> = scratch("TINYCHAT_MEDIA_HOSTS")
            .map(|hosts| hosts.split(',').map(|host| (host.to_owned(), fetch::Trust::Open)).collect())
            .unwrap_or_default();
        // The demo's files come from its own server on localhost.
        if crate::demo::seeded() {
            local.push(("127.0.0.1".to_owned(), fetch::Trust::Open));
        }
        let number = |name: &str| scratch(name).and_then(|value| value.parse::<u64>().ok());
        let cache = Arc::new(Cache {
            hub,
            dir,
            cap: number("TINYCHAT_MEDIA_CAP").unwrap_or(CACHE_BYTES),
            on_screen: number("TINYCHAT_MEDIA_ON_SCREEN").map_or(ON_SCREEN, Duration::from_secs),
            hosts: fetch::Hosts { local },
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            evicting: Mutex::new(()),
        });
        for worker in 0..IN_FLIGHT {
            let cache = cache.clone();
            // The first worker checks the directory before either fetches anything, so a file
            // on its way is never mistaken for one nobody knows.
            let spawned = std::thread::Builder::new().name("media".into()).spawn(move || {
                if worker == 0 {
                    cache.reconcile();
                    cache.evict();
                    crate::hub::lock(&cache.queue).ready = true;
                    cache.wake.notify_all();
                }
                cache.work();
            });
            if let Err(err) = spawned {
                eprintln!("chatd: media: {err}");
            }
        }
        Blobs { cache }
    }

    /// Where an account's files go.
    pub fn dir_of(&self, account: &AccountKey) -> PathBuf {
        dir_under(&self.cache.dir, account)
    }

    /// The shell has these files on screen. Answers at once with what is known, paths included
    /// for those on disk, and marks them used. Ids nobody registered are left out. A file that
    /// failed is answered with why and left alone. The rest are queued; each lands, or fails,
    /// as a `Change::Media`.
    ///
    /// `adapter` is the account's adapter, for `Command::Download`; `None` when the account is
    /// not connected, in which case only files that need no session can be fetched.
    pub fn request(
        &self,
        account: &AccountKey,
        ids: &[String],
        adapter: Option<Sender<Command>>,
    ) -> rusqlite::Result<Vec<MediaFile>> {
        let files: Vec<MediaFile> = {
            let mut store = self.cache.hub.store();
            store.media_used(account, ids, now())?;
            let mut files = Vec::with_capacity(ids.len());
            for (mut file, _) in store.media(account, ids)? {
                // Something else deleted it: it is simply not fetched yet.
                if file.path.as_ref().is_some_and(|path| !Path::new(path).exists()) {
                    store.media_evicted(account, &file.id)?;
                    file.path = None;
                }
                files.push(file);
            }
            files
        };
        let mut dropped = Vec::new();
        {
            let mut queue = crate::hub::lock(&self.cache.queue);
            let at = Instant::now();
            queue.asked.retain(|_, asked| at.duration_since(*asked) < self.cache.on_screen);
            let epoch = queue.epochs.get(account).copied().unwrap_or(0);
            for file in &files {
                let key = (account.clone(), file.id.clone());
                queue.asked.insert(key.clone(), at);
                // A failure stands until `Fetch` says `retry` (the server clears it before
                // this) or the adapter registers a fresh address.
                if file.path.is_some() || file.failed.is_some() || queue.fetching.contains(&key) {
                    continue;
                }
                // Asked for again while waiting: it goes back to the front.
                queue.waiting.retain(|job| job.file != key);
                queue.waiting.push(Job {
                    file: key,
                    epoch,
                    adapter: adapter.clone(),
                });
                if queue.waiting.len() > WAITING {
                    dropped.push(queue.waiting.remove(0).file);
                }
            }
        }
        self.cache.wake.notify_all();
        // Whoever asked for what gave way is told it did not arrive, so nothing waits on it.
        for (account, id) in dropped {
            let known = self.cache.hub.store().media(&account, &[id])?;
            for (file, _) in known {
                self.cache.announce(&account, file);
            }
        }
        Ok(files)
    }

    /// Brings the cache back under its cap. Runs after a fetch lands and at start.
    pub fn evict(&self) {
        self.cache.evict();
    }

    /// Sign-out: every file of the account goes, and whatever is on its way is dropped. The
    /// store's rows go with `Store::forget`.
    pub fn forget(&self, account: &AccountKey) {
        {
            let mut queue = crate::hub::lock(&self.cache.queue);
            *queue.epochs.entry(account.clone()).or_default() += 1;
            queue.waiting.retain(|job| job.file.0 != *account);
            queue.asked.retain(|(of, _), _| of != account);
            queue.landed.retain(|(of, _), _| of != account);
        }
        let _ = std::fs::remove_dir_all(self.dir_of(account));
    }
}

impl Cache {
    /// Tells the shell a file landed (`path` set) or will not (`failed` says why).
    fn announce(&self, account: &AccountKey, file: MediaFile) {
        self.hub.changed(Change::Media {
            account: account.clone(),
            file,
        });
    }

    /// Makes the directory and the store agree, once, at start. A file no row points to is
    /// removed, parts of interrupted downloads included; a row whose file is gone loses its path.
    fn reconcile(&self) {
        for network in Network::ALL {
            let Ok(accounts) = std::fs::read_dir(self.dir.join(network.label().to_lowercase())) else {
                continue;
            };
            for entry in accounts.flatten() {
                let account = AccountKey {
                    network,
                    account: entry.file_name().to_string_lossy().into_owned(),
                };
                let Ok(rows) = self.hub.store().media_on_disk(Some(&account), u32::MAX) else {
                    continue;
                };
                let known: HashSet<&std::ffi::OsStr> = rows.iter().filter_map(|row| Path::new(&row.path).file_name()).collect();
                let Ok(files) = std::fs::read_dir(entry.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    if !known.contains(file.file_name().as_os_str()) {
                        let _ = std::fs::remove_file(file.path());
                    }
                }
                // Only an empty directory goes: an account with nothing cached.
                let _ = std::fs::remove_dir(entry.path());
            }
        }
        let Ok(rows) = self.hub.store().media_on_disk(None, u32::MAX) else {
            return;
        };
        for row in rows.iter().filter(|row| !Path::new(&row.path).exists()) {
            let _ = self.hub.store().media_evicted(&row.account, &row.id);
        }
    }

    /// Deletes least recently used files until the cache is under its cap. What was asked for
    /// in the last minute stays, even if that leaves the cache over for a while.
    fn evict(&self) {
        // Two workers landing files at once need only one of them to do this.
        let Ok(_one) = self.evicting.try_lock() else {
            return;
        };
        let mut batch = 64;
        loop {
            let (mut total, oldest) = {
                let store = self.hub.store();
                match (store.media_bytes(), store.media_on_disk(None, batch)) {
                    (Ok(total), Ok(oldest)) => (total, oldest),
                    _ => return,
                }
            };
            if total <= self.cap {
                return;
            }
            let on_screen: HashSet<FileKey> = {
                let queue = crate::hub::lock(&self.queue);
                let recent = queue.asked.iter().filter(|(_, asked)| asked.elapsed() < self.on_screen);
                recent.map(|(key, _)| key.clone()).collect()
            };
            let listed = oldest.len();
            let mut freed = false;
            for file in oldest {
                if total <= self.cap {
                    return;
                }
                let key = (file.account, file.id);
                if on_screen.contains(&key) {
                    continue;
                }
                // Only what this cache put there is its to delete.
                if Path::new(&file.path).starts_with(&self.dir) {
                    let _ = std::fs::remove_file(&file.path);
                }
                if self.hub.store().media_evicted(&key.0, &key.1).is_err() {
                    return;
                }
                total = total.saturating_sub(file.bytes);
                freed = true;
            }
            // Everything listed was on screen: look further, if there is further to look.
            if !freed {
                if listed < batch as usize {
                    return;
                }
                batch = batch.saturating_mul(4);
            }
        }
    }

    /// One worker: takes the newest request, fetches it, and says how it went.
    fn work(&self) {
        let agent = fetch::agent();
        let mut buffer = vec![0u8; fetch::BUFFER];
        loop {
            let job = {
                let mut queue = crate::hub::lock(&self.queue);
                loop {
                    let next = if queue.ready { queue.waiting.pop() } else { None };
                    match next {
                        Some(job) => {
                            queue.fetching.push(job.file.clone());
                            break job;
                        }
                        None => queue = self.wake.wait(queue).unwrap_or_else(std::sync::PoisonError::into_inner),
                    }
                }
            };
            let outcome = self.fetch(&agent, &job, &mut buffer);
            let (account, id) = &job.file;
            crate::hub::lock(&self.queue).fetching.retain(|file| file != &job.file);
            match outcome {
                Some(Ok(file)) => {
                    self.announce(account, file);
                    self.evict();
                }
                Some(Err((mut file, reason))) => {
                    eprintln!("chatd: {}: media {id}: {}", account.network.label(), crate::adapters::net::redact(&reason));
                    // Recorded, so asking again answers with the reason and fetches nothing.
                    match self.hub.store().media_failed(account, id, &reason) {
                        Ok(Some(recorded)) => file = recorded,
                        Ok(None) => continue,
                        Err(_) => file.failed = Some(reason),
                    }
                    self.announce(account, file);
                }
                // The account signed out, or the file was forgotten: nobody is waiting.
                None => {}
            }
        }
    }

    fn signed_out_since(&self, job: &Job) -> bool {
        let queue = crate::hub::lock(&self.queue);
        queue.epochs.get(&job.file.0).copied().unwrap_or(0) != job.epoch
    }

    /// Up to [`RENEWED_TOGETHER`] other files of the account that are waiting their turn, as
    /// `(id, address)`: when one address has to be renewed, the adapter is told of these too
    /// and renews those that ran out in the same request.
    fn waiting_with(&self, job: &Job) -> Vec<(String, String)> {
        let ids: Vec<String> = {
            let queue = crate::hub::lock(&self.queue);
            let same = queue.waiting.iter().rev().filter(|other| other.file.0 == job.file.0);
            same.take(RENEWED_TOGETHER - 1).map(|other| other.file.1.clone()).collect()
        };
        let known = self.hub.store().media(&job.file.0, &ids).unwrap_or_default();
        known.into_iter().map(|(file, url)| (file.id, url)).collect()
    }

    /// Fetches one file. `Ok` carries it with its path; `Err` carries it without one and why.
    fn fetch(&self, agent: &ureq::Agent, job: &Job, buffer: &mut [u8]) -> Option<Result<MediaFile, (MediaFile, String)>> {
        let (account, id) = &job.file;
        let failed = |file: &MediaFile, why: String| Some(Err((file.clone(), why)));
        let part = dir_under(&self.dir, account).join(format!(".{}.part", name_of(id)));
        // Twice at most: a file the shell wanted larger while the smaller one was arriving is
        // fetched again from where the row points now.
        for last in [false, true] {
            // Read the row now, not when it was queued: the adapter may have registered a fresh
            // address since, and the account may be gone.
            let (file, url) = self.hub.store().media(account, std::slice::from_ref(id)).ok()?.pop()?;
            if self.signed_out_since(job) {
                return None;
            }
            if file.path.is_some() {
                return Some(Ok(file));
            }
            if let Err(err) = std::fs::create_dir_all(dir_under(&self.dir, account)) {
                return failed(&file, format!("The cache directory could not be made: {err}."));
            }
            let want = fetch::Want {
                network: account.network,
                file: &file,
                url: &url,
                adapter: job.adapter.as_ref(),
                part: &part,
                cap: fetch::cap(&file, self.cap),
                also: &|| self.waiting_with(job),
            };
            let (bytes, from) = match fetch::download(&self.hosts, agent, &want, buffer) {
                Ok(fetched) => fetched,
                Err(why) => return failed(&file, why),
            };
            if self.signed_out_since(job) {
                let _ = std::fs::remove_file(&part);
                return None;
            }
            // The row may name another size by now. A renewed address for the same picture
            // (`from`, which the adapter recorded) is not that.
            let now_at = self.hub.store().media(account, std::slice::from_ref(id)).ok()?.pop()?.1;
            if now_at != url && now_at != from && !last {
                let _ = std::fs::remove_file(&part);
                continue;
            }
            let path = path_from(&self.dir, account, id, &now_at);
            if let Err(err) = std::fs::rename(&part, &path) {
                let _ = std::fs::remove_file(&part);
                return failed(&file, format!("The file could not be kept: {err}."));
            }
            let landed = self.hub.store().media_landed(account, id, &path.to_string_lossy(), bytes, now());
            return match landed {
                Ok(Some(file)) => {
                    self.replaced(job, path);
                    Some(Ok(file))
                }
                // Forgotten while it was arriving.
                Ok(None) => {
                    let _ = std::fs::remove_file(&path);
                    None
                }
                Err(err) => {
                    let _ = std::fs::remove_file(&path);
                    failed(&file, format!("The file could not be recorded: {err}."))
                }
            };
        }
        None
    }

    /// Remembers where a file landed and removes the smaller size it replaces.
    fn replaced(&self, job: &Job, path: PathBuf) {
        let mut queue = crate::hub::lock(&self.queue);
        if queue.landed.len() >= REMEMBERED {
            queue.landed.clear();
        }
        let before = queue.landed.insert(job.file.clone(), path.clone());
        drop(queue);
        if let Some(before) = before.filter(|before| *before != path) {
            let _ = std::fs::remove_file(before);
        }
    }
}
