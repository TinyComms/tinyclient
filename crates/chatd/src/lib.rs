pub mod adapters;
pub mod blobs;
pub mod demo;
pub mod hub;
pub mod media;
pub mod notify;
pub mod playback;
pub mod server;
pub mod session;
pub mod store;
pub mod tls;
pub mod verified;

use protocol::{chat_sock_path_in, ChatListener};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// Who runs the core: a process of its own (`chatd`), or an app that starts it (`start_in_process`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Process,
    InProcess,
}

/// Starts chatd's core inside this process, on a thread of its own, over the socket for `dir`.
/// Returns once the socket listens; the thread serves until the process ends. Fails with
/// `AlreadyExists` while another chatd holds `dir`.
pub fn start_in_process(dir: PathBuf) -> io::Result<JoinHandle<()>> {
    spawn_core(dir, Host::InProcess, |_| Ok(()))
}

/// Opens the store, takes `dir`'s lock, binds the socket and serves on a thread of its own. `seed`
/// runs on the store first (the demo). Setup failures come back here and the thread ends; once
/// this returns `Ok`, the core is listening.
pub fn spawn_core(
    dir: PathBuf,
    host: Host,
    seed: impl FnOnce(&mut store::Store) -> io::Result<()> + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let (ready, answer) = std::sync::mpsc::channel::<io::Result<()>>();
    let spawned = std::thread::Builder::new().name("chatd".into()).stack_size(8 * 1024 * 1024).spawn(move || {
        let (lock, listener, chat) = match setup(&dir, host, seed) {
            Ok(parts) => parts,
            Err(err) => {
                let _ = ready.send(Err(err));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        // Held until serve ends: a second chatd for this directory finds it busy.
        let _lock = lock;
        if let Err(err) = server::serve(listener, chat) {
            tracing::warn!("chatd: {err}");
        }
    })?;
    match answer.recv() {
        Ok(Ok(())) => Ok(spawned),
        Ok(Err(err)) => {
            let _ = spawned.join();
            Err(err)
        }
        Err(_) => Err(io::Error::other("chatd stopped while starting")),
    }
}

fn setup(dir: &Path, host: Host, seed: impl FnOnce(&mut store::Store) -> io::Result<()>) -> io::Result<(File, ChatListener, server::Chat)> {
    std::fs::create_dir_all(dir)?;
    // One chatd per support directory. A shell starts chatd whenever it finds none answering,
    // so two may be started at once: the second finds the lock held and leaves the first be.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("chatd.lock"))?;
    if let Err(err) = lock.try_lock() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("another chatd is already running for {} ({err})", dir.display())));
    }
    #[cfg(target_os = "android")]
    session::app_file::install(dir);
    let mut store = store::Store::open(&dir.join("chat.sqlite")).map_err(io::Error::other)?;
    seed(&mut store)?;
    let path = chat_sock_path_in(dir);
    let listener = ChatListener::bind(&path)?;
    tracing::warn!("chatd listening on {}", path.display());
    let chat = match host {
        Host::Process => server::Chat::new(store),
        Host::InProcess => server::Chat::in_process(store),
    };
    Ok((lock, listener, chat))
}
