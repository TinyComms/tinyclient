//! Telegram's files. They come over the session, in pieces, not from an address the media cache
//! could fetch: so the pictures a row shows (thumbnails, stills, avatars) are fetched here, one
//! at a time, into the media cache's own directory, and recorded as the cache records a file
//! that landed. From then on they are the cache's: counted, evicted and deleted like any other.
//! A picture asked for as a row's (`Watch` says which rows are on screen) waits, unfetched,
//! once the row has scrolled away, and goes back in line when it is on screen again.
//!
//! The line has three parts. An original the viewer is waiting on goes first. Then the
//! pictures messages show (a photo's thumbnail, a video's or a sticker's still) that the shell
//! asked for because they are on screen (`Command::Wanted`), the latest asked first. Then
//! everything else, in the order pages named it, with rows' pictures that came back on screen
//! at its front. So a channel opened after another does not wait behind the first one's
//! pictures, nor behind its own that are not on screen yet. A picture the shell asks for before
//! a page has named it here goes ahead once a page does. A picker's tiles keep their turn: a
//! picker asks for a whole tab at once. The file being fetched is never cut short, and the
//! line never holds more than [`WAITING`]: past it, the last a page named is let go first.
//!
// CONTRACT: an adapter has no way to hand the media cache a file. This does what the cache does
// when one lands (`Store::register_media`, `Store::media_landed`, `Change::Media`) and nothing
// else. A file is registered only once it is on disk, so the cache never tries the address
// it cannot fetch and never marks the file failed.

use super::content::Want;
use super::wire::{self, Fault};
use super::{Account, Context};
use crate::adapters::net;
use crate::blobs;
use crate::hub::now;
use aes::Aes256;
use cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use grammers_tl_types as tl;
use protocol::{Change, Key, MediaKind};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Files waiting their turn. Past this, new ones are let go: they are asked for again when
/// their conversation is next opened.
const WAITING: usize = 256;
/// Bytes asked for at a time. Telegram wants a multiple of 4096.
const PIECE: i32 = 256 * 1024;
/// The least time between two file requests.
const PACE: std::time::Duration = std::time::Duration::from_millis(250);
/// While a row's picture waits for its row to come back, the screen is looked at this
/// often; the watch's `Look` wakes this sooner.
const PARKED_LOOK: Duration = Duration::from_secs(1);
/// A row's picture whose row has been away this long is no longer waited for.
const PARKED_FOR: Duration = Duration::from_secs(600);
/// The largest picture fetched for a row.
const LARGEST: u64 = 2 * 1024 * 1024;
/// What an explicit picture used to stop at. An open or a save is no longer held to it.
const LARGEST_ORIGINAL: u64 = 64 * 1024 * 1024;
/// What an explicit file used to stop at. An open or a save is no longer held to it.
const LARGEST_FILE: u64 = 200 * 1024 * 1024;
/// The most an explicit open or save will keep. Telegram serves up to 4 GB from a Premium sender.
const KEEP_LARGEST: u64 = 4 * 1024 * 1024 * 1024;
/// Telegram's pieces start on this boundary, and a resumed part does too.
const PIECE_ALIGN: u64 = 4_096;
/// Bytes asked for at a time for such a file, which the user is waiting on: the most Telegram
/// gives in one piece, at the same pace as everything else.
const FILE_PIECE: i32 = 1024 * 1024;
/// Files whose place is remembered until the shell asks for one: originals, and the pictures
/// messages show, about two for each photo. At most this many; the latest half always stays.
/// One forgotten comes back when a page that shows it is read again.
const HELD: usize = 8_192;
/// A file the shell asked for before any page named it here is still taken as asked for when
/// a page names it this soon after.
const EARLY_FOR: Duration = Duration::from_secs(30);
/// Files asked for before any page named them, remembered at most; the latest half always stays.
const EARLY: usize = 512;

enum Job {
    /// The connection files are fetched over from now on.
    Attach(Weak<Account>),
    Fetch(Box<Want>),
    /// The viewer is waiting on it: it goes before every row's picture.
    First(Box<Want>),
    /// A picture a message shows, which the shell asked for at that moment because it was on
    /// screen: it goes before every picture that is not, the latest asked first.
    Seen(Box<Want>, Instant),
    /// What is on screen changed: rows that came back put their pictures back in line.
    Look,
}

pub struct Files {
    ctx: Context,
    jobs: Sender<Job>,
    waiting: Arc<AtomicUsize>,
    held: Mutex<Held>,
}

/// What the shell may ask for by id, and what it asked for before it was known here.
struct Held {
    /// Where each original and each picture a message shows is, by its id.
    places: Recent<Want>,
    /// Files the shell asked for that no page had named yet, with when it asked.
    early: Recent<Instant>,
}

impl Default for Held {
    fn default() -> Held {
        Held { places: Recent::new(HELD / 2), early: Recent::new(EARLY / 2) }
    }
}

impl Held {
    /// A page named these files. The places of originals are kept and, on a conversation's
    /// page (`messages`), those of the pictures its messages show: such a picture the shell
    /// asked for a moment ago goes ahead as if it were asked now. Answers the jobs that go
    /// ahead, and what waits its turn. An original waits for the viewer.
    fn named(&mut self, wants: Vec<Want>, messages: bool) -> (Vec<Job>, Vec<Want>) {
        let (mut ahead, mut rest) = (Vec::new(), Vec::new());
        for want in wants {
            let picture = messages && message_picture(&want);
            if want.original || picture {
                self.places.insert(want.file.id.clone(), want.clone());
            }
            let asked = self.early.remove(&want.file.id).filter(|at| at.elapsed() < EARLY_FOR);
            match asked {
                Some(_) if want.original => ahead.push(Job::First(Box::new(want))),
                Some(at) if picture => ahead.push(Job::Seen(Box::new(want), at)),
                _ if want.original => {}
                _ => rest.push(want),
            }
        }
        (ahead, rest)
    }

    /// The shell asked for these at `at`: the jobs for those whose place is known. One no page
    /// has named here yet is remembered a while, and goes ahead if a page names it soon after.
    fn asked(&mut self, ids: &[String], at: Instant) -> Vec<Job> {
        let mut jobs = Vec::new();
        for id in ids {
            match self.places.get(id) {
                Some(want) => {
                    let want = Box::new(want.clone());
                    jobs.push(if want.original { Job::First(want) } else { Job::Seen(want, at) });
                }
                None => self.early.insert(id.clone(), at),
            }
        }
        jobs
    }
}

/// Values by id that forget the oldest first. Once the newer half is full it becomes the older
/// half, and the older half before it is let go: the latest `half` always stay, and never more
/// than twice that.
struct Recent<T> {
    half: usize,
    newer: HashMap<String, T>,
    older: HashMap<String, T>,
}

impl<T> Recent<T> {
    fn new(half: usize) -> Recent<T> {
        Recent { half, newer: HashMap::new(), older: HashMap::new() }
    }

    fn get(&self, id: &str) -> Option<&T> {
        self.newer.get(id).or_else(|| self.older.get(id))
    }

    fn insert(&mut self, id: String, value: T) {
        self.older.remove(&id);
        if self.newer.len() >= self.half && !self.newer.contains_key(&id) {
            self.older = std::mem::take(&mut self.newer);
        }
        self.newer.insert(id, value);
    }

    fn remove(&mut self, id: &str) -> Option<T> {
        let newer = self.newer.remove(id);
        let older = self.older.remove(id);
        newer.or(older)
    }
}

/// A picture a message shows in its row: a photo's thumbnail, a video's, a GIF's or a
/// sticker's still. Not an avatar or a custom emoji, which lists ask for by the hundred whether
/// they are in sight or not.
fn message_picture(want: &Want) -> bool {
    !want.original && want.file.kind == MediaKind::Image
}

/// What waits its turn, in the order it goes.
#[derive(Default)]
struct Line {
    /// Originals the viewer is waiting on, the latest asked first.
    first: VecDeque<Box<Want>>,
    /// Pictures the shell asked for while they were on screen, with when, the latest first.
    seen: VecDeque<(Instant, Box<Want>)>,
    /// Everything else, in the order pages named it.
    rest: VecDeque<Box<Want>>,
}

impl Line {
    fn len(&self) -> usize {
        self.first.len() + self.seen.len() + self.rest.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn pop(&mut self) -> Option<Box<Want>> {
        self.first.pop_front().or_else(|| self.seen.pop_front().map(|(_, want)| want)).or_else(|| self.rest.pop_front())
    }

    /// A page named it: at the back, or, if it is waiting already, where it waits, with the
    /// reference this page carried. `true` when it was waiting, so one fewer is counted.
    fn fetch(&mut self, want: Box<Want>) -> bool {
        let id = &want.file.id;
        let waiting = self.first.iter_mut().chain(self.seen.iter_mut().map(|(_, held)| held)).chain(self.rest.iter_mut()).find(|held| held.file.id == *id);
        match waiting {
            Some(held) => {
                // A reference runs out; the newest page's is the one that works.
                held.location = want.location;
                held.dc = want.dc;
                true
            }
            None => {
                self.rest.push_back(want);
                false
            }
        }
    }

    /// The viewer asked for it: first of all. `true` when it was waiting already.
    fn first(&mut self, want: Box<Want>) -> bool {
        let before = self.first.len();
        self.first.retain(|held| held.file.id != want.file.id);
        let waiting = self.first.len() < before;
        self.first.push_front(want);
        waiting
    }

    /// The shell asked for it at `at` because it was on screen: ahead of everything but the
    /// viewer's originals and what was asked for later. `true` when it was waiting already.
    fn seen(&mut self, want: Box<Want>, mut at: Instant) -> bool {
        let id = &want.file.id;
        let mut waiting = false;
        if let Some(found) = self.seen.iter().position(|(_, held)| held.file.id == *id) {
            if let Some((asked, _)) = self.seen.remove(found) {
                at = at.max(asked);
            }
            waiting = true;
        }
        if let Some(found) = self.rest.iter().position(|held| held.file.id == *id) {
            self.rest.remove(found);
            waiting = true;
        }
        // After what was asked at the same moment, so one ask keeps its own order.
        let place = self.seen.iter().position(|(asked, _)| *asked < at).unwrap_or(self.seen.len());
        self.seen.insert(place, (at, want));
        waiting
    }

    /// Lets go of what is past `most`: the last a page named first, then what was asked for
    /// earliest. It is named again when its conversation is next read. Answers how many went.
    fn trim(&mut self, most: usize) -> usize {
        let mut gone = 0;
        while self.len() > most && (self.rest.pop_back().is_some() || self.seen.pop_back().is_some()) {
            gone += 1;
        }
        gone
    }
}

impl Files {
    /// Starts the thread that fetches. It ends when the adapter's last connection is dropped.
    pub fn start(ctx: &Context) -> Files {
        let (jobs, inbox) = channel::<Job>();
        let waiting = Arc::new(AtomicUsize::new(0));
        let keeper = ctx.clone();
        let (ctx, counted) = (ctx.clone(), waiting.clone());
        let spawned = std::thread::Builder::new().name("telegram-files".into()).spawn(move || {
            let mut account = Weak::new();
            // Datacenters this connection's sign-in has been carried to.
            let mut carried = HashSet::new();
            let mut line = Line::default();
            // Rows' pictures asked for and since scrolled away, with when each was parked.
            let mut parked: Vec<(Instant, Box<Want>)> = Vec::new();
            let mut gone = false;
            loop {
                // Everything sent so far is read before the next fetch, so an original the
                // viewer asked for, or a picture on screen, does not wait behind the rest.
                let next = if !line.is_empty() {
                    inbox.try_recv().ok()
                } else if parked.is_empty() {
                    match inbox.recv() {
                        Ok(job) => Some(job),
                        Err(_) => {
                            gone = true;
                            None
                        }
                    }
                } else {
                    // Nothing to fetch while rows' pictures wait for their rows: the screen
                    // is looked at this often, and a `Look` wakes this at once.
                    match inbox.recv_timeout(PARKED_LOOK) {
                        Ok(job) => Some(job),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => {
                            gone = true;
                            None
                        }
                    }
                };
                if let Some(job) = next {
                    match job {
                        Job::Attach(to) => {
                            account = to;
                            carried.clear();
                        }
                        Job::Fetch(want) => {
                            if line.fetch(want) {
                                counted.fetch_sub(1, Ordering::Relaxed);
                            }
                        }
                        Job::First(want) => {
                            if line.first(want) {
                                counted.fetch_sub(1, Ordering::Relaxed);
                            }
                        }
                        Job::Seen(want, at) => {
                            if line.seen(want, at) {
                                counted.fetch_sub(1, Ordering::Relaxed);
                            }
                        }
                        Job::Look => rows_back(&account, &mut parked, &mut line.rest, &counted),
                    }
                    // What the shell asks for is never refused at the door, so the line keeps
                    // its own limit, parked pictures counted.
                    let let_go = line.trim(WAITING.saturating_sub(parked.len()));
                    counted.fetch_sub(let_go, Ordering::Relaxed);
                    continue;
                }
                // Nothing new at the wire: rows that came back go ahead of the rest, then the line.
                rows_back(&account, &mut parked, &mut line.rest, &counted);
                let Some(want) = line.pop() else {
                    // The wire is gone and nothing is left to fetch: the thread ends,
                    // whatever still waits for a row.
                    if gone {
                        return;
                    }
                    continue;
                };
                let Some(account) = account.upgrade() else {
                    counted.fetch_sub(1, Ordering::Relaxed);
                    continue;
                };
                if ctx.stopped() {
                    return;
                }
                // A row's picture whose row scrolled away waits for it to come back,
                // unfetched — still counted, so the resume cannot spend the same turn twice.
                if want.watched && want.row.as_deref().is_none_or(|row| !row_shown(&account, row)) {
                    parked.push((Instant::now(), want));
                    continue;
                }
                counted.fetch_sub(1, Ordering::Relaxed);
                if let Err(fault) = fetch(&account, &want, &mut carried) {
                    // A reference that ran out is renewed by the next page that names the file.
                    if !fault.is("FILE_REFERENCE_EXPIRED") {
                        tracing::warn!("chatd: Telegram: file {}: {fault}", want.file.id);
                    }
                    // The viewer is waiting on an original: it is told, and keeps the still.
                    if want.original {
                        let mut file = want.file.clone();
                        let what = if want.file.kind == MediaKind::Image { "the full picture" } else { "the file" };
                        file.failed = Some(format!("Telegram did not send {what}."));
                        ctx.hub.changed(Change::Media { account: ctx.account.clone(), file });
                    }
                }
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("chatd: Telegram: files: {err}");
        }
        Files { ctx: keeper, jobs, waiting, held: Mutex::default() }
    }

    pub fn attach(&self, account: &Arc<Account>) {
        let _ = self.jobs.send(Job::Attach(Arc::downgrade(account)));
    }

    /// What is on screen changed (`Watch`): rows whose pictures wait for their rows are
    /// looked at again, so one scrolled back goes first in line.
    pub fn look(&self) {
        let _ = self.jobs.send(Job::Look);
    }

    /// Queues what a conversation's page shows, and keeps the places of originals and of the
    /// pictures its messages show for [`Files::asked`]: such a picture the shell asked for before
    /// this goes ahead as if it were asked now. Nothing is fetched twice: a file waiting already
    /// keeps its one place, and a file already on disk is passed over when its turn comes. What
    /// came inside the message (a stripped preview, a cached size) is kept at once, here: it asks
    /// Telegram nothing, so it never waits in line behind the downloads it stands in for.
    pub fn want(&self, wants: Vec<Want>) {
        self.queue(wants, true);
    }

    /// Queues a picker's tiles, and keeps the originals' places. A tile keeps its turn when the
    /// shell asks for it: a picker asks for its whole tab at once, in sight or not.
    pub fn want_tiles(&self, wants: Vec<Want>) {
        self.queue(wants, false);
    }

    fn queue(&self, wants: Vec<Want>, messages: bool) {
        let (inline, wants): (Vec<Want>, Vec<Want>) = wants.into_iter().partition(|want| want.inline.is_some() && !want.original);
        for want in inline {
            let bytes = want.inline.as_deref().unwrap_or_default();
            if let Err(fault) = keep(&self.ctx, &want, |out, _| write_inline(out, bytes)) {
                tracing::warn!("chatd: Telegram: file {}: {fault}", want.file.id);
            }
        }
        let (ahead, rest) = crate::hub::lock(&self.held).named(wants, messages);
        self.ahead(ahead);
        for want in rest {
            if self.waiting.fetch_add(1, Ordering::Relaxed) >= WAITING {
                self.waiting.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            let _ = self.jobs.send(Job::Fetch(Box::new(want)));
        }
    }

    /// The shell asked for these files, which the media cache does not know, or knows only as
    /// a file Telegram kept that has since left the disk. An original among them is fetched
    /// next; a picture a message shows goes ahead of every picture not on screen. One no page
    /// has named yet goes ahead when a page does (`Held::asked`). Answers how many were known.
    pub fn asked(&self, ids: &[String]) -> usize {
        let jobs = crate::hub::lock(&self.held).asked(ids, Instant::now());
        let count = jobs.len();
        self.ahead(jobs);
        count
    }

    /// Sends what the shell asked for: never refused here, however many wait. The fetching
    /// thread keeps the line to [`WAITING`] by letting go of what matters least (`Line::trim`).
    fn ahead(&self, jobs: Vec<Job>) {
        for job in jobs {
            self.waiting.fetch_add(1, Ordering::Relaxed);
            let _ = self.jobs.send(job);
        }
    }
}

fn media_dir() -> PathBuf {
    protocol::support_dir().join("media")
}

/// Whether the row this picture hangs on is on screen now. The shell says (`Watch`); the
/// open conversation is on screen whatever the list shows.
fn row_shown(account: &Account, row: &str) -> bool {
    let watching = account.ctx.watching();
    let shown = |key: &Key| key.account() == account.ctx.account && key.id == row;
    watching.rows.iter().any(shown) || watching.conversation.as_ref().is_some_and(|open| shown(open))
}

/// Rows that came back put their pictures back in line, at the front of what is waiting its
/// turn (`queue`, the line's `rest`: behind the viewer's originals and the pictures the shell
/// asked for on screen), in the order they were parked; what is still away stays parked, and
/// what has waited too long goes: the shell asks again if the row is still there.
fn rows_back(account: &Weak<Account>, parked: &mut Vec<(Instant, Box<Want>)>, queue: &mut VecDeque<Box<Want>>, counted: &AtomicUsize) {
    // A parked picture still occupies its place among the waiting files. Release it only
    // when it is discarded, or when the queue takes it to fetch after its row returns.
    parked.retain(|(at, _)| {
        let keep = at.elapsed() < PARKED_FOR;
        if !keep {
            counted.fetch_sub(1, Ordering::Relaxed);
        }
        keep
    });
    let Some(account) = account.upgrade() else {
        return;
    };
    let (back, kept): (Vec<_>, Vec<_>) = std::mem::take(parked).into_iter().partition(|(_, want)| {
        want.row.as_deref().is_some_and(|row| row_shown(&account, row))
    });
    *parked = kept;
    for (_, want) in back.into_iter().rev() {
        queue.push_front(want);
    }
}

/// One file, from Telegram to the cache's directory.
fn fetch(account: &Account, want: &Want, carried: &mut HashSet<i32>) -> Result<(), Fault> {
    let written = keep(&account.ctx, want, |out, start| match &want.inline {
        Some(bytes) => write_inline(out, bytes),
        None => download(account, want, carried, start, out),
    })?;
    if written {
        account.keep_session();
    }
    Ok(())
}

fn local(err: std::io::Error) -> Fault {
    Fault::Net(format!("the file could not be kept: {err}"))
}

/// The bytes a message carried, as the file they are.
fn write_inline(out: &mut std::fs::File, bytes: &[u8]) -> Result<u64, Fault> {
    out.write_all(bytes).map(|()| bytes.len() as u64).map_err(local)
}

/// One file into the cache's directory, written by `write`, and recorded as the cache records
/// a file that landed. `false` when it was on disk already and nothing was written.
fn keep(ctx: &Context, want: &Want, write: impl FnOnce(&mut std::fs::File, u64) -> Result<u64, Fault>) -> Result<bool, Fault> {
    let id = std::slice::from_ref(&want.file.id);
    let on_disk = |ctx: &Context| {
        let known = ctx.hub.store().media(&ctx.account, id).unwrap_or_default();
        known.first().and_then(|(file, _)| file.path.clone()).is_some_and(|path| std::path::Path::new(&path).exists())
    };
    // What an older fetch kept as its gzip container is not what a shell reads: thrown
    // away, so the fetch of this turn keeps the file inside.
    if want.file.kind == MediaKind::Emoji {
        if let Some(held) = ctx.hub.store().media(&ctx.account, id).unwrap_or_default().first().and_then(|(file, _)| file.path.clone()).map(std::path::PathBuf::from) {
            if std::fs::read(&held).is_ok_and(|bytes| bytes.len() >= 2 && bytes[..2] == [0x1f, 0x8b]) {
                let _ = std::fs::remove_file(&held);
            }
        }
    }
    if on_disk(ctx) {
        return Ok(false);
    }
    // Inline previews each get their own part so a page and a live update can write the same
    // preview at once without cutting each other short. A fetched file keeps one part so a
    // dropped download can continue from where it stopped.
    static PARTS: AtomicUsize = AtomicUsize::new(0);
    let path = blobs::path_of(&media_dir(), &ctx.account, &want.file.id);
    std::fs::create_dir_all(blobs::dir_under(&media_dir(), &ctx.account)).map_err(local)?;
    let (part, mut out, start) = if want.inline.is_some() {
        let part = path.with_extension(format!("part{}", PARTS.fetch_add(1, Ordering::Relaxed)));
        let file = std::fs::File::create(&part).map_err(local)?;
        (part, file, 0)
    } else {
        // Read as well as write: a CDN piece is checked against its checksum before the next one.
        let part = path.with_extension("part");
        let start = resume_len(&part);
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part).map_err(local)?;
        file.seek(SeekFrom::Start(start)).map_err(local)?;
        (part, file, start)
    };
    let written = write(&mut out, start);
    drop(out);
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(fault) => {
            // A checksum failure is not a place to continue from. Anything else leaves the
            // part, and the next ask carries on from its length.
            if fault.to_string().contains("did not match") {
                let _ = std::fs::remove_file(&part);
            }
            return Err(fault);
        }
    };
    // Many emoji files (a topic's icon among them) are served gzipped: what is kept
    // is the file inside, the picture or animation a shell knows how to draw.
    let bytes = match want.file.kind {
        MediaKind::Emoji => {
            let unpacked = std::fs::read(&part).ok().and_then(|held| net::gunzip(&held, LARGEST as usize));
            match unpacked {
                Some(unpacked) => {
                    let _ = std::fs::write(&part, &unpacked);
                    unpacked.len() as u64
                }
                None => bytes,
            }
        }
        _ => bytes,
    };
    if let Err(err) = std::fs::rename(&part, &path) {
        let _ = std::fs::remove_file(&part);
        return Err(local(err));
    }
    let stored = |err: rusqlite::Error| Fault::Net(err.to_string());
    // The address is only a name for where it came from; nothing fetches it.
    let address = format!("tg://file/{}", want.file.id);
    let landed = {
        let mut store = ctx.hub.store();
        store.register_media(&ctx.account, &[(want.file.clone(), address)]).map_err(stored)?;
        store.media_landed(&ctx.account, &want.file.id, &path.to_string_lossy(), bytes, now()).map_err(stored)?
    };
    if let Some(file) = landed {
        ctx.hub.changed(Change::Media { account: ctx.account.clone(), file });
        // A picture is part of a row, which named no file while there was none: the row and
        // the people list are read again.
        if want.file.kind == MediaKind::Avatar {
            ctx.hub.changed(Change::People { account: ctx.account.clone() });
        }
        if let Some(row) = &want.row {
            ctx.hub.changed(Change::Conversation { conversation: ctx.key(row) });
        }
    }
    Ok(true)
}

/// How far this fetch may go. A file the user opened or saved is kept up to Telegram's own
/// largest. A picture fetched for a row stays small.
pub(super) fn most_bytes(original: bool, bytes: u64) -> u64 {
    if !original {
        return LARGEST;
    }
    if bytes == 0 {
        return KEEP_LARGEST;
    }
    bytes.saturating_add(bytes / 8).saturating_add(FILE_PIECE as u64).clamp(LARGEST, KEEP_LARGEST)
}

/// A partial download continues from the last whole piece. A torn tail is cut so the next
/// request starts where Telegram's pieces do.
fn resume_len(part: &std::path::Path) -> u64 {
    let len = std::fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
    let aligned = len - (len % PIECE_ALIGN);
    if aligned != len {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(part) {
            let _ = file.set_len(aligned);
        }
    }
    aligned
}

fn too_large(want: &Want) -> Fault {
    let name = if want.file.kind == MediaKind::Image { "picture" } else { "file" };
    let why = if want.original {
        format!("the {name} is larger than tinyclient can keep")
    } else {
        format!("the {name} is larger than tinyclient downloads on its own")
    };
    Fault::Net(why)
}

fn mismatched() -> Fault {
    Fault::Net("Telegram's copy of this file did not match.".to_owned())
}

type AesCtr = Ctr128BE<Aes256>;

/// The counter Telegram's CDN uses: the redirect's IV plus `offset / 16`, big-endian.
pub(super) fn cdn_iv(iv: &[u8; 16], offset: u64) -> [u8; 16] {
    let block = u128::from_be_bytes(*iv).wrapping_add(u128::from(offset / 16));
    block.to_be_bytes()
}

/// AES-256-CTR over one CDN piece. The same call encrypts and decrypts.
pub(in crate::adapters::telegram) fn cdn_apply(key: &[u8], iv: &[u8; 16], offset: u64, bytes: &mut [u8]) -> Result<(), Fault> {
    if key.len() != 32 {
        return Err(mismatched());
    }
    let counter = cdn_iv(iv, offset);
    let mut cipher = AesCtr::new(key.into(), counter.as_slice().into());
    let skip = (offset % 16) as usize;
    if skip != 0 {
        let mut junk = vec![0u8; skip];
        cipher.apply_keystream(&mut junk);
    }
    cipher.apply_keystream(bytes);
    Ok(())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hash_end(hash: &tl::types::FileHash) -> u64 {
    (hash.offset.max(0) as u64).saturating_add(hash.limit.max(0) as u64)
}

/// The pieces of one file, in order, from the datacenter that holds it. `start` is the part
/// already on disk.
fn download(account: &Account, want: &Want, carried: &mut HashSet<i32>, start: u64, out: &mut std::fs::File) -> Result<u64, Fault> {
    let wire = account.wire.as_ref();
    let home = wire.home();
    let dc = (want.dc != 0 && want.dc != home).then_some(want.dc);
    if let Some(dc) = dc {
        // A file kept elsewhere is asked of that datacenter, which first has to be told who we
        // are: the home datacenter vouches once per connection. A CDN redirect is not this
        // datacenter, and a CDN does not take that voucher.
        if !carried.contains(&dc) {
            let tl::enums::auth::ExportedAuthorization::Authorization(pass) =
                account.ask(&tl::functions::auth::ExportAuthorization { dc_id: dc })?;
            wire::call_in(wire, Some(dc), &tl::functions::auth::ImportAuthorization { id: pass.id, bytes: pass.bytes })?;
            carried.insert(dc);
        }
    }
    // A picture's pieces stay small; a file the user pressed play on comes in the largest.
    let limit = match (want.original, want.file.kind) {
        (true, MediaKind::Image) | (false, _) => PIECE,
        (true, _) => FILE_PIECE,
    };
    let most = most_bytes(want.original, want.file.bytes);
    let mut offset = start as i64;
    loop {
        // Pictures for a scrolled list are many small requests: paced, never back to back, the
        // way a busy client still stays well under Telegram's flood limits.
        if account.ctx.pause(PACE) {
            return Err(Fault::Net("stopped".to_owned()));
        }
        let request = tl::functions::upload::GetFile {
            precise: false,
            cdn_supported: true,
            location: want.location.clone(),
            offset,
            limit,
        };
        let pause = |time| account.ctx.pause(time);
        let piece = match wire::call_in(wire, dc, &request) {
            // A flood wait is honored in full here as everywhere.
            Err(fault) => match fault.flood() {
                Some(seconds) if seconds <= wire::PATIENCE && !pause(std::time::Duration::from_secs(seconds)) => {
                    wire::call_in(wire, dc, &request)?
                }
                _ => return Err(fault),
            },
            Ok(piece) => piece,
        };
        match piece {
            tl::enums::upload::File::File(piece) => {
                out.write_all(&piece.bytes).map_err(|err| Fault::Net(err.to_string()))?;
                offset += piece.bytes.len() as i64;
                if (piece.bytes.len() as i32) < limit {
                    return Ok(offset as u64);
                }
                if offset as u64 > most {
                    return Err(too_large(want));
                }
            }
            tl::enums::upload::File::CdnRedirect(redirect) => {
                return download_cdn(account, &redirect, offset as u64, most, want, out);
            }
        }
    }
}

fn flatten_hashes(hashes: Vec<tl::enums::FileHash>) -> Vec<tl::types::FileHash> {
    hashes.into_iter().map(|hash| match hash { tl::enums::FileHash::Hash(hash) => hash }).collect()
}

/// The ranges the home datacenter says this CDN file covers. The CDN is not asked: it is the
/// thing the hashes check.
fn cdn_hash_list(account: &Account, token: &[u8], redirected: &[tl::enums::FileHash]) -> Result<Vec<tl::types::FileHash>, Fault> {
    let asked = account.ask(&tl::functions::upload::GetCdnFileHashes { file_token: token.to_vec(), offset: 0 })?;
    let hashes = flatten_hashes(asked);
    if !hashes.is_empty() {
        return Ok(hashes);
    }
    let hashes = flatten_hashes(redirected.to_vec());
    if hashes.is_empty() { Err(mismatched()) } else { Ok(hashes) }
}

fn check_hashes(out: &mut std::fs::File, hashes: &[tl::types::FileHash], end: u64) -> Result<(), Fault> {
    out.flush().map_err(|err| Fault::Net(err.to_string()))?;
    let pos = out.stream_position().map_err(|err| Fault::Net(err.to_string()))?;
    for hash in hashes {
        let from = hash.offset.max(0) as u64;
        let span = hash.limit.max(0) as u64;
        if span == 0 || from.saturating_add(span) > end {
            continue;
        }
        let mut buf = vec![0u8; span as usize];
        out.seek(SeekFrom::Start(from)).map_err(|err| Fault::Net(err.to_string()))?;
        out.read_exact(&mut buf).map_err(|_| mismatched())?;
        if sha256(&buf).as_slice() != hash.hash.as_slice() {
            return Err(mismatched());
        }
    }
    out.seek(SeekFrom::Start(pos)).map_err(|err| Fault::Net(err.to_string()))?;
    Ok(())
}

fn download_cdn(
    account: &Account,
    redirect: &tl::types::upload::FileCdnRedirect,
    start: u64,
    most: u64,
    want: &Want,
    out: &mut std::fs::File,
) -> Result<u64, Fault> {
    let key: [u8; 32] = redirect.encryption_key.as_slice().try_into().map_err(|_| mismatched())?;
    let iv: [u8; 16] = redirect.encryption_iv.as_slice().try_into().map_err(|_| mismatched())?;
    let hashes = cdn_hash_list(account, &redirect.file_token, &redirect.file_hashes)?;
    let wire = account.wire.as_ref();
    let limit = FILE_PIECE;
    let mut offset = start;
    let mut reuploaded = false;
    loop {
        if account.ctx.pause(PACE) {
            return Err(Fault::Net("stopped".to_owned()));
        }
        let request = tl::functions::upload::GetCdnFile {
            file_token: redirect.file_token.clone(),
            offset: offset as i64,
            limit,
        };
        // The CDN datacenter is not signed into. It answers `getCdnFile` and nothing else.
        let piece = match wire::call_in(wire, Some(redirect.dc_id), &request)? {
            tl::enums::upload::CdnFile::File(file) => file.bytes,
            tl::enums::upload::CdnFile::ReuploadNeeded(need) if !reuploaded => {
                reuploaded = true;
                let _ = account.ask(&tl::functions::upload::ReuploadCdnFile {
                    file_token: redirect.file_token.clone(),
                    request_token: need.request_token,
                })?;
                continue;
            }
            tl::enums::upload::CdnFile::ReuploadNeeded(_) => return Err(mismatched()),
        };
        if piece.is_empty() {
            if hashes.iter().any(|hash| hash.limit > 0 && hash_end(hash) > offset) {
                return Err(mismatched());
            }
            return Ok(offset);
        }
        let mut plain = piece;
        cdn_apply(&key, &iv, offset, &mut plain)?;
        out.write_all(&plain).map_err(|err| Fault::Net(err.to_string()))?;
        let end = offset + plain.len() as u64;
        check_hashes(out, &hashes, end)?;
        offset = end;
        if (plain.len() as i32) < limit {
            if hashes.iter().any(|hash| hash.limit > 0 && hash_end(hash) > offset) {
                return Err(mismatched());
            }
            return Ok(offset);
        }
        if offset > most {
            return Err(too_large(want));
        }
    }
}

/// The lab's telegram-large-files check. A 300 MB file the user opened is not stopped at
/// 200 MB, an automatic fetch still is, and a CDN piece decrypts to the bytes its checksum names.
pub(super) fn prove_large_files() -> Result<(), String> {
    let big = 300 * 1024 * 1024;
    let cap = most_bytes(true, big);
    if cap < big || cap <= LARGEST_FILE {
        return Err(format!("a 300 MB file the user opened is capped at {cap}"));
    }
    if most_bytes(false, big) != LARGEST {
        return Err("an automatic fetch is still capped".into());
    }
    if most_bytes(true, 80 * 1024 * 1024) <= LARGEST_ORIGINAL {
        return Err("an opened picture is still held to 64 MB".into());
    }
    let key = [3u8; 32];
    let iv = [5u8; 16];
    let plain: [u8; 32] = std::array::from_fn(|at| (at as u8).wrapping_add(1));
    let mut hidden = plain;
    cdn_apply(&key, &iv, 0, &mut hidden).map_err(|err| err.to_string())?;
    let mut back = hidden;
    cdn_apply(&key, &iv, 0, &mut back).map_err(|err| err.to_string())?;
    if back != plain || sha256(&back) != sha256(&plain) {
        return Err("a CDN file did not decrypt to its checksum".into());
    }
    let mut tail = hidden[16..].to_vec();
    cdn_apply(&key, &iv, 16, &mut tail).map_err(|err| err.to_string())?;
    if tail.as_slice() != &plain[16..] {
        return Err("a CDN piece past the first block did not decrypt".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parked_picture_releases_its_waiting_slot_once_when_it_times_out() {
        let want = || Box::new(Want {
            file: protocol::MediaFile::new("avatar", MediaKind::Avatar),
            dc: 0,
            location: tl::types::InputPhotoFileLocation {
                id: 1, access_hash: 0, file_reference: Vec::new(), thumb_size: "s".into(),
            }.into(),
            inline: None,
            row: Some("chat".into()),
            watched: true,
            original: false,
        });
        let mut parked = vec![(Instant::now() - PARKED_FOR, want()), (Instant::now(), want())];
        let mut queue = VecDeque::new();
        let counted = AtomicUsize::new(2);
        rows_back(&Weak::new(), &mut parked, &mut queue, &counted);
        assert_eq!(counted.load(Ordering::Relaxed), 1);
        assert_eq!(parked.len(), 1);
        assert!(queue.is_empty());
        rows_back(&Weak::new(), &mut parked, &mut queue, &counted);
        assert_eq!(counted.load(Ordering::Relaxed), 1);
    }

    /// A file of `kind` named `id`, with `reference` as its file reference.
    fn file(id: &str, kind: MediaKind, original: bool, reference: u8) -> Box<Want> {
        Box::new(Want {
            file: protocol::MediaFile::new(id, kind),
            dc: 4,
            location: tl::types::InputPhotoFileLocation {
                id: 1, access_hash: 0, file_reference: vec![reference], thumb_size: "m".into(),
            }.into(),
            inline: None,
            row: None,
            watched: false,
            original,
        })
    }

    fn thumb(id: &str) -> Box<Want> {
        file(id, MediaKind::Image, false, 1)
    }

    fn order(mut line: Line) -> Vec<String> {
        std::iter::from_fn(|| line.pop()).map(|want| want.file.id).collect()
    }

    #[test]
    fn a_picture_on_screen_goes_ahead_of_the_rest_and_behind_the_original_the_viewer_waits_on() {
        let mut line = Line::default();
        // The channel left: its page's pictures, in the order they were named.
        for id in ["a1", "a2", "a3"] {
            assert!(!line.fetch(thumb(id)));
        }
        // The channel opened now: its page goes behind them, and its picture on screen ahead.
        assert!(!line.fetch(thumb("b1")));
        assert!(!line.fetch(thumb("b2")));
        assert!(line.seen(thumb("b2"), Instant::now()), "it was waiting: counted once");
        assert!(!line.first(file("original", MediaKind::Image, true, 1)));
        assert_eq!(order(line), ["original", "b2", "a1", "a2", "a3", "b1"]);
    }

    #[test]
    fn the_latest_asked_goes_first_and_what_was_asked_together_keeps_its_order() {
        let mut line = Line::default();
        let then = Instant::now();
        let now = then + Duration::from_millis(5);
        // The first channel's pictures on screen, asked for together.
        assert!(!line.seen(thumb("a1"), then));
        assert!(!line.seen(thumb("a2"), then));
        // The next channel's, asked for later: ahead of them.
        assert!(!line.seen(thumb("b1"), now));
        assert!(!line.seen(thumb("b2"), now));
        // A page naming a picture asked for before is taken at when it was asked.
        assert!(!line.seen(thumb("early"), then));
        assert_eq!(order(line), ["b1", "b2", "a1", "a2", "early"]);
    }

    #[test]
    fn a_file_named_or_asked_for_again_keeps_one_place_with_the_newest_reference() {
        let mut line = Line::default();
        let at = Instant::now();
        assert!(!line.fetch(thumb("a1")));
        assert!(!line.seen(thumb("b1"), at));
        // Named again by a newer page: where it waits, with that page's reference.
        assert!(line.fetch(file("a1", MediaKind::Image, false, 2)));
        assert!(line.fetch(file("b1", MediaKind::Image, false, 3)));
        // Asked for twice: one place, the later ask's, with the place `want` last kept for it.
        assert!(line.seen(file("b1", MediaKind::Image, false, 4), at + Duration::from_millis(1)));
        // The viewer asked twice: one place, first.
        assert!(!line.first(file("original", MediaKind::Image, true, 1)));
        assert!(line.first(file("original", MediaKind::Image, true, 1)));
        let reference = |want: &Want| match &want.location {
            tl::enums::InputFileLocation::InputPhotoFileLocation(photo) => photo.file_reference.clone(),
            _ => Vec::new(),
        };
        let mut taken = Vec::new();
        while let Some(want) = line.pop() {
            taken.push((want.file.id.clone(), reference(&want)));
        }
        assert_eq!(taken, [("original".into(), vec![1]), ("b1".into(), vec![4]), ("a1".into(), vec![2])]);
    }

    #[test]
    fn past_its_limit_the_line_lets_go_of_the_last_named_then_the_earliest_asked() {
        let mut line = Line::default();
        let then = Instant::now();
        for id in ["a1", "a2", "a3"] {
            line.fetch(thumb(id));
        }
        line.seen(thumb("b1"), then);
        line.seen(thumb("b2"), then + Duration::from_millis(1));
        line.first(file("original", MediaKind::Image, true, 1));
        assert_eq!(line.trim(6), 0);
        assert_eq!(line.trim(4), 2);
        assert_eq!(line.trim(2), 2, "then what was asked for earliest");
        // The viewer's original is never let go for the limit.
        assert_eq!(line.trim(0), 1);
        assert_eq!(order(line), ["original"]);
    }

    #[test]
    fn what_is_remembered_keeps_the_latest_half_and_a_name_read_again_stays() {
        let mut recent = Recent::new(2);
        let held = |recent: &Recent<u8>| ["a", "b", "c", "d", "e"].into_iter().filter(|id| recent.get(id).is_some()).collect::<Vec<_>>();
        for (id, value) in [("a", 1), ("b", 1), ("c", 1), ("d", 1)] {
            recent.insert(id.to_owned(), value);
        }
        // a and b moved to the older half when c came; nothing is let go yet.
        assert_eq!(held(&recent), ["a", "b", "c", "d"]);
        // Named again, a is one of the latest: it does not go with its old half. The newer half
        // was full, so it became the older one, and b went.
        recent.insert("a".to_owned(), 2);
        assert_eq!(held(&recent), ["a", "c", "d"]);
        assert_eq!(recent.get("a"), Some(&2));
        recent.insert("e".to_owned(), 1);
        assert_eq!(held(&recent), ["a", "c", "d", "e"]);
        // Never fewer than the latest two, never more than four.
        recent.insert("b".to_owned(), 1);
        assert_eq!(held(&recent), ["a", "b", "e"]);
        assert_eq!(recent.remove("a"), Some(2));
        assert_eq!(held(&recent), ["b", "e"]);
    }

    /// Jobs as (kind, file).
    fn jobs(jobs: Vec<Job>) -> Vec<(&'static str, String)> {
        jobs.into_iter().map(|job| match job {
            Job::Fetch(want) => ("fetch", want.file.id),
            Job::First(want) => ("first", want.file.id),
            Job::Seen(want, _) => ("seen", want.file.id),
            Job::Attach(_) => ("attach", String::new()),
            Job::Look => ("look", String::new()),
        }).collect()
    }

    /// What a page naming these sends ahead, and what waits its turn: a conversation's page,
    /// or a picker's (`messages` false).
    fn named_by(held: &mut Held, wants: Vec<Box<Want>>, messages: bool) -> (Vec<(&'static str, String)>, Vec<String>) {
        let (ahead, rest) = held.named(wants.into_iter().map(|want| *want).collect(), messages);
        (jobs(ahead), rest.into_iter().map(|want| want.file.id).collect())
    }

    fn named(held: &mut Held, wants: Vec<Box<Want>>) -> (Vec<(&'static str, String)>, Vec<String>) {
        named_by(held, wants, true)
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn a_picture_the_shell_asks_for_before_its_page_is_read_goes_ahead_when_it_is() {
        let mut held = Held::default();
        // On screen from the store before the page is read again: nothing is known of them.
        assert!(held.asked(&ids(&["p2m", "a5"]), Instant::now()).is_empty());
        // The picture asked for goes ahead, past any limit on what waits; an avatar keeps its
        // turn; the original waits for the viewer.
        let page = vec![thumb("p1m"), thumb("p2m"), file("a5", MediaKind::Avatar, false, 1), file("p2y", MediaKind::Image, true, 1)];
        assert_eq!(named(&mut held, page), (vec![("seen", "p2m".into())], ids(&["p1m", "a5"])));
        // Asked for now that the page is known: a message's picture goes ahead, an original
        // first, and an avatar is not a message's picture.
        assert_eq!(jobs(held.asked(&ids(&["p1m", "p2y", "a5"]), Instant::now())), [("seen", "p1m".into()), ("first", "p2y".into())]);
        // An ask answered from what was known is not remembered: a page read again soon after
        // takes its turn, and only the one asked for early went ahead, once.
        assert_eq!(named(&mut held, vec![thumb("p1m"), thumb("p2m")]), (vec![], ids(&["p1m", "p2m"])));
    }

    #[test]
    fn a_pickers_tiles_keep_their_turn_however_the_shell_asks() {
        let mut held = Held::default();
        // The picker asks for its whole tab, some before the catalog was read.
        assert!(held.asked(&ids(&["d1m"]), Instant::now()).is_empty());
        let tab = vec![thumb("d1m"), thumb("d2m"), file("d2", MediaKind::Gif, true, 1)];
        assert_eq!(named_by(&mut held, tab, false), (vec![], ids(&["d1m", "d2m"])));
        // Asked for again once the catalog is known: still nothing goes ahead but an original.
        assert_eq!(jobs(held.asked(&ids(&["d2m", "d2"]), Instant::now())), [("first", "d2".into())]);
    }

    #[test]
    fn an_ask_goes_ahead_only_while_it_is_recent() {
        let mut held = Held::default();
        held.asked(&["p1m".into()], Instant::now() - EARLY_FOR);
        assert_eq!(named(&mut held, vec![thumb("p1m")]), (vec![], vec!["p1m".to_owned()]));
    }

    #[test]
    fn an_opened_file_may_pass_the_old_ceiling_and_a_row_fetch_may_not() {
        let big = 300 * 1024 * 1024;
        let cap = most_bytes(true, big);
        assert!(cap >= big && cap > LARGEST_FILE, "{cap}");
        assert_eq!(most_bytes(false, big), LARGEST);
        assert!(most_bytes(true, 80 * 1024 * 1024) > LARGEST_ORIGINAL);
    }

    #[test]
    fn a_torn_part_resumes_on_a_piece_boundary() {
        let path = std::env::temp_dir().join(format!(
            "tinyclient-part-{}-{}.part",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&path, vec![1u8; 5_000]).unwrap();
        assert_eq!(resume_len(&path), 4_096);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 4_096);
        let _ = std::fs::remove_file(&path);
    }
}
