//! Telegram's files. They come over the session, in pieces, not from an address the media cache
//! could fetch: so the pictures a row shows (thumbnails, stills, avatars) are fetched here, one
//! at a time, into the media cache's own directory, and recorded as the cache records a file
//! that landed. From then on they are the cache's: counted, evicted and deleted like any other.
//! A picture asked for as a row's (`Watch` says which rows are on screen) waits, unfetched,
//! once the row has scrolled away, and goes back in line when it is on screen again.
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
/// Originals whose place is remembered until the viewer asks for one. Past this they are
/// forgotten, and come back when a page that shows them is read again.
const HELD: usize = 4_096;

enum Job {
    /// The connection files are fetched over from now on.
    Attach(Weak<Account>),
    Fetch(Box<Want>),
    /// The viewer is waiting on it: it goes before every row's picture.
    First(Box<Want>),
    /// What is on screen changed: rows that came back put their pictures back in line.
    Look,
}

pub struct Files {
    jobs: Sender<Job>,
    waiting: Arc<AtomicUsize>,
    /// Where each original is, by its id, until the shell asks for it.
    held: Mutex<HashMap<String, Want>>,
}

impl Files {
    /// Starts the thread that fetches. It ends when the adapter's last connection is dropped.
    pub fn start(ctx: &Context) -> Files {
        let (jobs, inbox) = channel::<Job>();
        let waiting = Arc::new(AtomicUsize::new(0));
        let (ctx, counted) = (ctx.clone(), waiting.clone());
        let spawned = std::thread::Builder::new().name("telegram-files".into()).spawn(move || {
            let mut account = Weak::new();
            // Datacenters this connection's sign-in has been carried to.
            let mut carried = HashSet::new();
            let mut queue: VecDeque<Box<Want>> = VecDeque::new();
            // Rows' pictures asked for and since scrolled away, with when each was parked.
            let mut parked: Vec<(Instant, Box<Want>)> = Vec::new();
            let mut gone = false;
            loop {
                // Everything sent so far is read before the next fetch, so an original the
                // viewer asked for does not wait behind the rows' pictures.
                let next = if !queue.is_empty() {
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
                        Job::Fetch(want) => queue.push_back(want),
                        Job::First(want) => queue.push_front(want),
                        Job::Look => rows_back(&account, &mut parked, &mut queue, &counted),
                    }
                    continue;
                }
                // Nothing new at the wire: rows that came back go first, then the queue.
                rows_back(&account, &mut parked, &mut queue, &counted);
                let Some(want) = queue.pop_front() else {
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
        Files { jobs, waiting, held: Mutex::default() }
    }

    pub fn attach(&self, account: &Arc<Account>) {
        let _ = self.jobs.send(Job::Attach(Arc::downgrade(account)));
    }

    /// What is on screen changed (`Watch`): rows whose pictures wait for their rows are
    /// looked at again, so one scrolled back goes first in line.
    pub fn look(&self) {
        let _ = self.jobs.send(Job::Look);
    }

    /// Queues what rows on screen show, and keeps the originals' places for [`Files::asked`].
    /// Nothing is fetched twice: a file already on disk is passed over when its turn comes.
    pub fn want(&self, wants: Vec<Want>) {
        let (originals, wants): (Vec<Want>, Vec<Want>) = wants.into_iter().partition(|want| want.original);
        if !originals.is_empty() {
            let mut held = crate::hub::lock(&self.held);
            if held.len() + originals.len() > HELD {
                held.clear();
            }
            held.extend(originals.into_iter().map(|want| (want.file.id.clone(), want)));
        }
        for want in wants {
            if self.waiting.fetch_add(1, Ordering::Relaxed) >= WAITING {
                self.waiting.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            let _ = self.jobs.send(Job::Fetch(Box::new(want)));
        }
    }

    /// The shell asked for these files, which the media cache does not know: the originals
    /// among them are fetched next. Answers how many were.
    pub fn asked(&self, ids: &[String]) -> usize {
        let found: Vec<Want> = {
            let held = crate::hub::lock(&self.held);
            ids.iter().filter_map(|id| held.get(id).cloned()).collect()
        };
        let count = found.len();
        for want in found {
            self.waiting.fetch_add(1, Ordering::Relaxed);
            let _ = self.jobs.send(Job::First(Box::new(want)));
        }
        count
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

/// Rows that came back put their pictures back in line, at its front, in the order they
/// were parked; what is still away stays parked, and what has waited too long goes: the
/// shell asks again if the row is still there.
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
    let ctx = &account.ctx;
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
        return Ok(());
    }
    let local = |err: std::io::Error| Fault::Net(format!("the file could not be kept: {err}"));
    let path = blobs::path_of(&media_dir(), &ctx.account, &want.file.id);
    let part = path.with_extension("part");
    std::fs::create_dir_all(blobs::dir_under(&media_dir(), &ctx.account)).map_err(local)?;
    // A file already started continues from the part. An inline picture is whole in one write.
    let (mut out, start) = if want.inline.is_some() {
        (std::fs::File::create(&part).map_err(local)?, 0)
    } else {
        // Read as well as write: a CDN piece is checked against its checksum before the next one.
        let start = resume_len(&part);
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part).map_err(local)?;
        file.seek(SeekFrom::Start(start)).map_err(local)?;
        (file, start)
    };
    let written = match &want.inline {
        Some(bytes) => out.write_all(bytes).map(|()| bytes.len() as u64).map_err(local),
        None => download(account, want, carried, start, &mut out),
    };
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
    account.keep_session();
    Ok(())
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
