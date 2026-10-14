//! Crash protection for the window. A panic is written, redacted, to `crash.log` by the shared
//! hook; when it happened on the main thread (where GPUI's callbacks run, so the process is about
//! to abort across AppKit) the shell starts itself again and the new window says so in a toast
//! that stays until dismissed: "tinyclient quit unexpectedly and reopened." with "Copy report".
//!
//! A relaunch passes the crash's time in `TINYCLIENT_CRASHED`. A second crash within
//! [`LOOP_GUARD`] of the first does not relaunch, so a panic at startup cannot loop.

use protocol::crash::{install_panic_hook, CRASH_LOG};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Set on the relaunched shell: the unix second of the crash it follows.
pub const CRASHED_ENV: &str = "TINYCLIENT_CRASHED";
pub const LOOP_GUARD: Duration = Duration::from_secs(60);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Whether a panic at `now` should start a new shell, given the crash this one followed.
pub fn should_relaunch(main_thread: bool, previous: Option<u64>, now: u64) -> bool {
    main_thread && previous.is_none_or(|before| now.saturating_sub(before) >= LOOP_GUARD.as_secs())
}

/// The crash this shell was relaunched after, if any.
pub fn relaunched_after() -> Option<u64> {
    std::env::var(CRASHED_ENV).ok()?.parse().ok()
}

/// Installs the shared hook, then adds the relaunch. First thing in `main`.
pub fn install() {
    install_panic_hook("tinyclient");
    let logged = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logged(info);
        let main_thread = std::thread::current().name() == Some("main");
        let at = now();
        // A screenshot run is a test: it must never leave a window behind.
        if cfg!(feature = "gui-shot") || !should_relaunch(main_thread, relaunched_after(), at) {
            return;
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut command = std::process::Command::new(exe);
            command.args(std::env::args_os().skip(1)).env(CRASHED_ENV, at.to_string());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let _ = command.spawn();
        }
    }));
}

/// The last entry of `crash.log` written by the shell, for "Copy report". Entries begin with
/// "<process> panicked at unix".
pub fn last_report(log: &str) -> Option<&str> {
    let starts = |line: &str| line.split_once(" panicked at unix ").is_some_and(|(process, _)| !process.contains(' '));
    let start = log.rfind("tinyclient panicked at unix")?;
    let rest = &log[start..];
    // The entry runs to the next line that starts another one.
    let mut end = rest.len();
    let mut at = rest.find('\n').map_or(rest.len(), |n| n + 1);
    while at < rest.len() {
        let line = rest[at..].lines().next().unwrap_or("");
        if starts(line) {
            end = at;
            break;
        }
        at += line.len() + 1;
    }
    Some(rest[..end].trim_end())
}

/// The shell's last crash report from disk. Call it off the main thread.
pub fn read_report() -> Option<String> {
    let log = std::fs::read_to_string(protocol::support_dir().join(CRASH_LOG)).ok()?;
    last_report(&log).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_main_thread_panic_relaunches_once_and_never_loops() {
        assert!(should_relaunch(true, None, 1000));
        // A thread that is not main ends alone; the window stays.
        assert!(!should_relaunch(false, None, 1000));
        // A crash right after a relaunch: stop.
        assert!(!should_relaunch(true, Some(990), 1000));
        assert!(should_relaunch(true, Some(900), 1000));
    }

    #[test]
    fn the_report_is_the_shells_last_entry() {
        let log = "chatd panicked at unix 1 on thread x: a\nbt\ntinyclient panicked at unix 2 on thread main: b\nbt2\n\
                   chatd panicked at unix 3 on thread y: c\n";
        assert_eq!(last_report(log), Some("tinyclient panicked at unix 2 on thread main: b\nbt2"));
        assert_eq!(last_report("chatd panicked at unix 1: x\n"), None);
        assert_eq!(last_report("tinyclient panicked at unix 5 on thread main: z\n"), Some("tinyclient panicked at unix 5 on thread main: z"));
    }
}
