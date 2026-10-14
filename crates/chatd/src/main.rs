use chatd::server::{serve, Chat};
use chatd::store::Store;
use protocol::{chat_sock_path, support_dir, ChatListener};
use std::io;

fn main() -> io::Result<()> {
    protocol::crash::install_panic_hook("chatd");
    let dir = support_dir();
    std::fs::create_dir_all(&dir)?;
    // One chatd per support directory. A shell starts chatd whenever it finds none answering,
    // so two may be started at once: the second finds the lock held and leaves the first be.
    // Held until the process exits.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("chatd.lock"))?;
    if let Err(err) = lock.try_lock() {
        eprintln!("chatd: another chatd is already running for {} ({err})", dir.display());
        return Ok(());
    }
    let mut store = Store::open(&dir.join("chat.sqlite")).map_err(io::Error::other)?;

    if let Some(mode) = chatd::demo::requested(std::env::args()) {
        if std::env::var_os("TINYCHAT_DIR").is_none() {
            eprintln!("chatd: --seed-demo needs TINYCHAT_DIR so it cannot write to the real store");
            std::process::exit(64);
        }
        chatd::demo::seed(&mut store, &dir, mode).map_err(io::Error::other)?;
    }

    let path = chat_sock_path();
    let listener = ChatListener::bind(&path)?;
    eprintln!("chatd listening on {}", path.display());
    serve(listener, Chat::new(store))
}
