//! How a shell finds and starts chatd, so nobody ever has to run it by hand.
//!
//! The shell's side, in order:
//!
//! 1. At launch, `Ping` with a short timeout ([`chatd_answers`]). No answer: [`start_chatd`],
//!    then keep asking `Status`. chatd answers its socket before it reads the keychain, so an
//!    answer comes within a second; `Status.keychain` then says whether it is waiting on macOS.
//! 2. When a request fails because the socket is gone (`NotFound`, `ConnectionRefused`, a
//!    broken pipe), keep what is on screen, say "Reconnecting…", and start chatd again. Give up
//!    after three starts within 30 s and say so.
//! 3. chatd restarted when `Status.started` differs from the last one seen, or a `Wait` answers
//!    `reset`: read everything on screen again.
//!
//! Starting twice is harmless: a second chatd finds the first one's lock and exits.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Names the chatd binary to start, overriding the rule below.
pub const CHATD_ENV: &str = "TINYCHAT_CHATD";
/// chatd's stderr, under [`crate::support_dir`]. Cut to nothing past [`CHATD_LOG_LIMIT`] at each
/// start.
pub const CHATD_LOG: &str = "chatd.log";
pub const CHATD_LOG_LIMIT: u64 = 4 * 1024 * 1024;

/// Where chatd is: `TINYCHAT_CHATD` when set, else beside the running executable. That is
/// `tinychat.app/Contents/MacOS/chatd` in the bundle (`packaging/bundle.sh` puts every binary
/// there) and `target/<profile>/chatd` under cargo, which builds it with `cargo build -p chatd`.
/// `None` when there is no such file.
pub fn chatd_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CHATD_ENV) {
        return Some(PathBuf::from(path));
    }
    let beside = std::env::current_exe().ok()?.with_file_name(format!("chatd{}", std::env::consts::EXE_SUFFIX));
    beside.is_file().then_some(beside)
}

/// Whether chatd answers a `Ping` within `timeout`.
pub fn chatd_answers(timeout: Duration) -> bool {
    let Ok(mut stream) = crate::ChatStream::connect(&crate::chat_sock_path()) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    crate::write_frame(&mut stream, &crate::Request::Ping).is_ok()
        && matches!(crate::read_frame(&mut stream), Ok(crate::Response::Pong))
}

/// Starts chatd in its own process group, so it outlives the shell and a terminal's ^C, with its
/// stderr appended to `chatd.log`. It inherits the environment (`TINYCHAT_DIR` included). The
/// child is reaped on a thread of its own. Answers its pid.
pub fn start_chatd() -> io::Result<u32> {
    let path = chatd_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "chatd is not beside tinychat"))?;
    let dir = crate::support_dir();
    std::fs::create_dir_all(&dir)?;
    let log = dir.join(CHATD_LOG);
    if std::fs::metadata(&log).is_ok_and(|meta| meta.len() > CHATD_LOG_LIMIT) {
        let _ = std::fs::remove_file(&log);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut command = std::process::Command::new(path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    // Own process group so the daemon outlives the shell and a terminal's ^C.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP
        command.creation_flags(0x0000_0200);
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    let _ = std::thread::Builder::new().name("chatd-reaper".into()).stack_size(64 * 1024).spawn(move || child.wait());
    Ok(pid)
}
