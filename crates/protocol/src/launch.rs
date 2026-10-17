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
pub const CHATD_ENV: &str = "TINYCLIENT_CHATD";
/// chatd's stderr, under [`crate::support_dir`]. Cut to nothing past [`CHATD_LOG_LIMIT`] at each
/// start.
pub const CHATD_LOG: &str = "chatd.log";
pub const CHATD_LOG_LIMIT: u64 = 4 * 1024 * 1024;

/// Where chatd is: `TINYCLIENT_CHATD` when set, else beside the running executable. That is
/// `tinyclient.app/Contents/MacOS/chatd` in the bundle (`packaging/bundle.sh` puts every binary
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

/// Whether the user enabled systemd management of the default Linux daemon.
/// Scratch directories and explicit binary overrides always use direct startup.
pub fn systemd_chatd() -> bool {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("TINYCLIENT_DIR").is_some() || std::env::var_os(CHATD_ENV).is_some() {
            return false;
        }
        return systemd_enabled(std::process::Command::new("systemctl"));
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "linux")]
fn systemd_enabled(mut command: std::process::Command) -> bool {
    command.args(["--user", "is-enabled", "tinyclient-chatd.service"])
        .stderr(std::process::Stdio::null()).output().is_ok_and(|output| {
            output.status.success()
                && matches!(output.stdout.trim_ascii(), b"enabled" | b"enabled-runtime")
        })
}

#[cfg(target_os = "linux")]
fn start_systemd(mut command: std::process::Command) -> io::Result<()> {
    // The GUI already polls the socket for readiness; do not wait for a systemd job.
    let status = command.args(["--user", "--no-block", "start", "tinyclient-chatd.service"])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("systemd could not start tinyclient-chatd.service; see journalctl --user -u tinyclient-chatd.service"))
    }
}

/// Starts chatd in its own process group, so it outlives the shell and a terminal's ^C, with its
/// stderr appended to `chatd.log`. It inherits the environment (`TINYCLIENT_DIR` included). The
/// child is reaped on a thread of its own. Answers its pid. With the optional user
/// unit enabled, asks systemd to start it instead and returns no child pid. A failed
/// service start never falls back to an unmanaged daemon.
pub fn start_chatd() -> io::Result<Option<u32>> {
    #[cfg(target_os = "linux")]
    if systemd_chatd() {
        start_systemd(std::process::Command::new("systemctl"))?;
        return Ok(None);
    }
    let path = chatd_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "chatd is not beside tinyclient"))?;
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
    // CREATE_NEW_PROCESS_GROUP, for the same reason.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0200 | CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    let _ = std::thread::Builder::new().name("chatd-reaper".into()).stack_size(64 * 1024).spawn(move || child.wait());
    Ok(Some(pid))
}

/// Windows's CREATE_NO_WINDOW: a console program started with it gets a console nobody sees.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts a helper (mediad, the sign-in window) without a console window of its own. They are
/// console programs, and started from the windowed shell or from chatd each would open one on
/// Windows. Redirected stdin, stdout and stderr still reach the helper. Nothing elsewhere.
pub fn no_console(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn shell(script: &str) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn only_enabled_units_opt_in_to_systemd() {
        for state in ["enabled", "enabled-runtime"] {
            assert!(systemd_enabled(shell(&format!("printf '%s\\n' '{state}'"))));
        }
        for state in ["disabled", "static", "indirect", "masked", "not-found", ""] {
            assert!(!systemd_enabled(shell(&format!("printf '%s\\n' '{state}'"))));
        }
        assert!(!systemd_enabled(shell("echo enabled; exit 1")));
        assert!(!systemd_enabled(std::process::Command::new("/nonexistent/systemctl")));
    }

    #[test]
    fn systemd_queries_and_starts_only_the_user_unit() {
        assert!(systemd_enabled(shell(
            "test \"$0\" = --user && test \"$1\" = is-enabled && test \"$2\" = tinyclient-chatd.service && echo enabled"
        )));
        start_systemd(shell(
            "test \"$0\" = --user && test \"$1\" = --no-block && test \"$2\" = start && test \"$3\" = tinyclient-chatd.service"
        )).unwrap();
    }

    #[test]
    fn a_failed_service_start_is_an_error() {
        let error = start_systemd(shell("exit 1")).unwrap_err();
        assert!(error.to_string().contains("journalctl --user"));
        assert!(start_systemd(std::process::Command::new("/nonexistent/systemctl")).is_err());
    }
}
