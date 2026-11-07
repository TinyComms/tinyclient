use chatd::{spawn_core, Host};
use std::io::{self, Write};

fn main() -> io::Result<()> {
    // Before the support directory is touched: the packages' smoke tests run it on a bare system.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("chatd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-reauth-history") {
        if let Err(err) = chatd::adapters::tiny::prove_reauthenticated_history() {
            eprintln!("tiny-reauth-history: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-tiny-reauth-history:0");
        return Ok(());
    }
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-direct-created") {
        if let Err(err) = chatd::adapters::tiny::prove_tiny_direct_created() {
            eprintln!("tiny-direct-created: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's leave-chime driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("leave-chime") {
        if let Err(err) = chatd::leave_prove::run() {
            eprintln!("leave-chime: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's join-order driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("join-before-room") {
        if let Err(err) = chatd::adapters::tiny::prove_join_before_room() {
            eprintln!("join-before-room: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's sealed-row driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-row") {
        if let Err(err) = chatd::adapters::tiny::prove_missing_seal() {
            eprintln!("sealed-row: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's sealed-preview driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-preview") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_preview() {
            eprintln!("sealed-preview: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's first-encrypted-message driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("first-encrypted-message") {
        if let Err(err) = chatd::adapters::tiny::prove_first_encrypted_message() {
            eprintln!("first-encrypted-message: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // Exercise an open through the real Tiny adapter command loop.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-open-backfill") {
        if let Err(err) = chatd::adapters::tiny::prove_tiny_open_backfill() {
            eprintln!("tiny-open-backfill: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's cache-before-enrol driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-before-enrol") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_before_enrol() {
            eprintln!("cache-before-enrol: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's cache-open-without-device driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-open-without-device") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_open_without_device() {
            eprintln!("cache-open-without-device: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's sealed-cache-preview driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-cache-preview") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_cache_preview() {
            eprintln!("sealed-cache-preview: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's sealed-preview-attachments driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-preview-attachments") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_preview_attachments() {
            eprintln!("sealed-preview-attachments: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // A torn cache tail must not absorb the next complete encrypted record.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-append-tail") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_append_tail() {
            eprintln!("cache-append-tail: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-cache-append-tail:0");
        return Ok(());
    }
    // The lab's keep-opened-sealed driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("keep-opened-sealed") {
        if let Err(err) = chatd::adapters::tiny::prove_keep_opened_sealed() {
            eprintln!("keep-opened-sealed: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-media-preserved:0");
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's refused-list driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("refused-list") {
        if let Err(err) = chatd::adapters::tiny::prove_refused_list() {
            eprintln!("refused-list: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's empty-list driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("empty-list") {
        if let Err(err) = chatd::adapters::tiny::prove_empty_list() {
            eprintln!("empty-list: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's before-blank driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("before-blank") {
        if let Err(err) = chatd::server::prove_before_blank() {
            eprintln!("before-blank: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's around-blank driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("around-blank") {
        if let Err(err) = chatd::server::prove_around_blank() {
            eprintln!("around-blank: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's kept-backfill driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("kept-backfill") {
        if let Err(err) = chatd::server::prove_kept_backfill() {
            eprintln!("kept-backfill: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's failed-start-offline driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("failed-start-offline") {
        if let Err(err) = chatd::adapters::prove_failed_start_offline() {
            eprintln!("failed-start-offline: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's signed-out-messages driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signed-out-messages") {
        if let Err(err) = chatd::server::prove_signed_out_messages() {
            eprintln!("signed-out-messages: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's signed-out-threads driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signed-out-threads") {
        if let Err(err) = chatd::server::prove_signed_out_threads() {
            eprintln!("signed-out-threads: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's connect-keeps-offline driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("connect-keeps-offline") {
        if let Err(err) = chatd::server::prove_connect_keeps_offline() {
            eprintln!("connect-keeps-offline: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let _log = protocol::log_policy::install("chatd", protocol::log_policy::LogPolicy::from_env());
    protocol::crash::install_panic_hook("chatd");
    let dir = protocol::support_dir();
    let demo = chatd::demo::requested(std::env::args());
    let seed_dir = dir.clone();
    let core = spawn_core(dir, Host::Process, move |store| {
        if let Some(mode) = demo {
            if std::env::var_os("TINYCLIENT_DIR").is_none() {
                tracing::warn!("chatd: --seed-demo needs TINYCLIENT_DIR so it cannot write to the real store");
                std::process::exit(64);
            }
            chatd::demo::seed(store, &seed_dir, mode).map_err(io::Error::other)?;
        }
        Ok(())
    });
    let core = match core {
        Ok(core) => core,
        // A shell starts chatd whenever it finds none answering, so two may start at once: the
        // second finds the first one's lock and leaves it be. The operator file gets the path.
        // Stderr always gets the short line: this process is about to exit, and the launcher
        // reads stderr even when it is not a terminal.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            tracing::warn!("chatd: {err}");
            let _ = writeln!(io::stderr(), "chatd: another chatd is already running");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    // The core serves until the process exits: a `Shutdown` exits it.
    core.join().map_err(|_| io::Error::other("chatd stopped"))
}
