use chatd::{spawn_core, Host};
use std::io;

fn main() -> io::Result<()> {
    // Before the support directory is touched: the packages' smoke tests run it on a bare system.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("chatd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    protocol::crash::install_panic_hook("chatd");
    let dir = protocol::support_dir();
    let demo = chatd::demo::requested(std::env::args());
    let seed_dir = dir.clone();
    let core = spawn_core(dir, Host::Process, move |store| {
        if let Some(mode) = demo {
            if std::env::var_os("TINYCLIENT_DIR").is_none() {
                eprintln!("chatd: --seed-demo needs TINYCLIENT_DIR so it cannot write to the real store");
                std::process::exit(64);
            }
            chatd::demo::seed(store, &seed_dir, mode).map_err(io::Error::other)?;
        }
        Ok(())
    });
    let core = match core {
        Ok(core) => core,
        // A shell starts chatd whenever it finds none answering, so two may start at once: the
        // second finds the first one's lock and leaves it be.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            eprintln!("chatd: {err}");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    // The core serves until the process exits: a `Shutdown` exits it.
    core.join().map_err(|_| io::Error::other("chatd stopped"))
}
