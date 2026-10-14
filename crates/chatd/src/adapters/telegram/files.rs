//! Telegram's files. They come over the session, in pieces, not from an address the media cache
//! could fetch: so the pictures a row shows (thumbnails, stills, avatars) are fetched here, one
//! at a time, into the media cache's own directory, and recorded as the cache records a file
//! that landed. From then on they are the cache's: counted, evicted and deleted like any other.
//!
// CONTRACT: an adapter has no way to hand the media cache a file. This does what the cache does
// when one lands (`Store::register_media`, `Store::media_landed`, `Change::Media`) and nothing
// else. A file is registered only once it is on disk, so the cache never tries the address
// it cannot fetch and never marks the file failed.

use super::content::Want;
use super::wire::{self, Fault};
use super::{Account, Context};
use crate::blobs;
use crate::hub::now;
use grammers_tl_types as tl;
use protocol::{Change, MediaKind};
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Weak};

/// Files waiting their turn. Past this, new ones are let go: they are asked for again when
/// their conversation is next opened.
const WAITING: usize = 256;
/// Bytes asked for at a time. Telegram wants a multiple of 4096.
const PIECE: i32 = 256 * 1024;
/// The largest picture fetched for a row.
const LARGEST: u64 = 2 * 1024 * 1024;

enum Job {
    /// The connection files are fetched over from now on.
    Attach(Weak<Account>),
    Fetch(Box<Want>),
}

pub struct Files {
    jobs: Sender<Job>,
    waiting: Arc<AtomicUsize>,
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
            for job in inbox {
                let want = match job {
                    Job::Attach(to) => {
                        account = to;
                        carried.clear();
                        continue;
                    }
                    Job::Fetch(want) => want,
                };
                counted.fetch_sub(1, Ordering::Relaxed);
                let Some(account) = account.upgrade() else {
                    continue;
                };
                if ctx.stopped() {
                    return;
                }
                if let Err(fault) = fetch(&account, &want, &mut carried) {
                    // A reference that ran out is renewed by the next page that names the file.
                    if !fault.is("FILE_REFERENCE_EXPIRED") {
                        eprintln!("chatd: Telegram: file {}: {fault}", want.file.id);
                    }
                }
            }
        });
        if let Err(err) = spawned {
            eprintln!("chatd: Telegram: files: {err}");
        }
        Files { jobs, waiting }
    }

    pub fn attach(&self, account: &Arc<Account>) {
        let _ = self.jobs.send(Job::Attach(Arc::downgrade(account)));
    }

    /// Queues what rows on screen show. Nothing is fetched twice: a file already on disk is
    /// passed over when its turn comes.
    pub fn want(&self, wants: Vec<Want>) {
        for want in wants {
            if self.waiting.fetch_add(1, Ordering::Relaxed) >= WAITING {
                self.waiting.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            let _ = self.jobs.send(Job::Fetch(Box::new(want)));
        }
    }
}

fn media_dir() -> PathBuf {
    protocol::support_dir().join("media")
}

/// One file, from Telegram to the cache's directory.
fn fetch(account: &Account, want: &Want, carried: &mut HashSet<i32>) -> Result<(), Fault> {
    let ctx = &account.ctx;
    let id = std::slice::from_ref(&want.file.id);
    let on_disk = |ctx: &Context| {
        let known = ctx.hub.store().media(&ctx.account, id).unwrap_or_default();
        known.first().and_then(|(file, _)| file.path.clone()).is_some_and(|path| std::path::Path::new(&path).exists())
    };
    if on_disk(ctx) {
        return Ok(());
    }
    let local = |err: std::io::Error| Fault::Net(format!("the file could not be kept: {err}"));
    let path = blobs::path_of(&media_dir(), &ctx.account, &want.file.id);
    let part = path.with_extension("part");
    std::fs::create_dir_all(blobs::dir_under(&media_dir(), &ctx.account)).map_err(local)?;
    let mut out = std::fs::File::create(&part).map_err(local)?;
    let written = match &want.inline {
        Some(bytes) => out.write_all(bytes).map(|()| bytes.len() as u64).map_err(local),
        None => download(account, want, carried, &mut out),
    };
    drop(out);
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(fault) => {
            let _ = std::fs::remove_file(&part);
            return Err(fault);
        }
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

/// The pieces of one file, in order, from the datacenter that holds it.
fn download(account: &Account, want: &Want, carried: &mut HashSet<i32>, out: &mut std::fs::File) -> Result<u64, Fault> {
    let wire = account.wire.as_ref();
    let home = wire.home();
    let dc = (want.dc != 0 && want.dc != home).then_some(want.dc);
    if let Some(dc) = dc {
        // A file kept elsewhere is asked of that datacenter, which first has to be told who we
        // are: the home datacenter vouches once per connection.
        if !carried.contains(&dc) {
            let tl::enums::auth::ExportedAuthorization::Authorization(pass) =
                account.ask(&tl::functions::auth::ExportAuthorization { dc_id: dc })?;
            wire::call_in(wire, Some(dc), &tl::functions::auth::ImportAuthorization { id: pass.id, bytes: pass.bytes })?;
            carried.insert(dc);
        }
    }
    let mut offset = 0i64;
    loop {
        let request = tl::functions::upload::GetFile {
            precise: false,
            cdn_supported: false,
            location: want.location.clone(),
            offset,
            limit: PIECE,
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
        let tl::enums::upload::File::File(piece) = piece else {
            return Err(Fault::Net("Telegram keeps this file where tinychat cannot fetch it yet".to_owned()));
        };
        out.write_all(&piece.bytes).map_err(|err| Fault::Net(err.to_string()))?;
        offset += piece.bytes.len() as i64;
        if (piece.bytes.len() as i32) < PIECE {
            return Ok(offset as u64);
        }
        if offset as u64 > LARGEST {
            return Err(Fault::Net("the picture is larger than a row needs".to_owned()));
        }
    }
}
