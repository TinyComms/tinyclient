//! Streamer mode helpers shared between chatd (notification silencing) and the shell
//! (message redaction, profile masking, capture exclusion, and broadcast-app detection).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Invite URL fragments that expose private rooms/servers if shown on stream.
const INVITE_PATTERNS: &[&str] = &[
    "discord.gg/",
    "discord.com/invite/",
    "discordapp.com/invite/",
    "t.me/+",
    "t.me/joinchat/",
    "signal.group/#",
    "tinyclient://join",
    "join.slack.com/",
    "teams.microsoft.com/l/meetup-join/",
];

/// Whether `text` carries something that should stay hidden in Streamer mode until clicked:
/// a sign-in / one-time verification code or a private invite link.
pub fn contains_sensitive(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if crate::otp::extract_code(text).is_some() {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    INVITE_PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

/// Whether a process executable name belongs to OBS Studio or Streamlabs Desktop.
pub fn is_broadcast_process(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let base = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    let stem = base.strip_suffix(".exe").unwrap_or(&base);
    matches!(
        stem,
        "obs"
            | "obs32"
            | "obs64"
            | "obs-studio"
            | "streamlabs"
            | "streamlabs obs"
            | "streamlabs desktop"
            | "streamlabs-desktop"
    )
}

const OBS_CACHE_TTL: Duration = Duration::from_secs(5);

/// Whether OBS Studio or Streamlabs Desktop is currently running on this machine.
/// Cached briefly so frequent render/notification checks never scan the process table
/// on every frame or message.
pub fn obs_running() -> bool {
    static CACHE: OnceLock<Mutex<Option<(bool, Instant)>>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    let now = Instant::now();
    if let Ok(guard) = lock.lock() {
        if let Some((running, at)) = *guard {
            if now.saturating_duration_since(at) < OBS_CACHE_TTL {
                return running;
            }
        }
    }
    let running = probe_obs_running();
    if let Ok(mut guard) = lock.lock() {
        *guard = Some((running, now));
    }
    running
}

#[cfg(windows)]
fn probe_obs_running() -> bool {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> isize;
        fn Process32FirstW(h_snapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(h_snapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(h_object: isize) -> i32;
    }

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == 0 || snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry = ProcessEntry32W {
            dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
            cnt_usage: 0,
            th32_process_id: 0,
            th32_default_heap_id: 0,
            th32_module_id: 0,
            cnt_threads: 0,
            th32_parent_process_id: 0,
            pc_pri_class_base: 0,
            dw_flags: 0,
            sz_exe_file: [0; 260],
        };
        let mut found = false;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry
                    .sz_exe_file
                    .iter()
                    .position(|&ch| ch == 0)
                    .unwrap_or(entry.sz_exe_file.len());
                let name = String::from_utf16_lossy(&entry.sz_exe_file[..len]);
                if is_broadcast_process(&name) {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(target_os = "macos")]
fn probe_obs_running() -> bool {
    use std::ffi::c_void;

    #[link(name = "proc")]
    extern "C" {
        fn proc_listallpids(buffer: *mut c_void, buffersize: i32) -> i32;
        fn proc_name(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    }

    unsafe {
        let count = proc_listallpids(std::ptr::null_mut(), 0);
        if count <= 0 {
            return false;
        }
        let mut pids = vec![0i32; (count as usize).saturating_add(32)];
        let bytes = pids.len().saturating_mul(std::mem::size_of::<i32>());
        let Ok(bytes_i32) = i32::try_from(bytes) else {
            return false;
        };
        let filled = proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes_i32);
        if filled <= 0 {
            return false;
        }
        let n = (filled as usize).min(pids.len());
        let mut name_buf = [0u8; 256];
        for &pid in &pids[..n] {
            if pid <= 0 {
                continue;
            }
            let len = proc_name(pid, name_buf.as_mut_ptr().cast::<c_void>(), name_buf.len() as u32);
            if len > 0 {
                let len = (len as usize).min(name_buf.len());
                if let Ok(name) = std::str::from_utf8(&name_buf[..len]) {
                    if is_broadcast_process(name) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(target_os = "linux")]
fn probe_obs_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(pid) = file.to_str() else { continue };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
            if is_broadcast_process(&comm) {
                return true;
            }
        }
        // `comm` keeps 15 bytes. The executable path in `cmdline` is the full name.
        if let Ok(cmd) = std::fs::read(entry.path().join("cmdline")) {
            let first = cmd.split(|byte| *byte == 0).next().unwrap_or(&[]);
            if let Ok(name) = std::str::from_utf8(first) {
                if is_broadcast_process(name) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn probe_obs_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_detection_catches_invite_links_and_sign_in_codes() {
        assert!(contains_sensitive("Join us at https://discord.gg/abc1234"));
        assert!(contains_sensitive("Server invite: https://discord.com/invite/tinycomms"));
        assert!(contains_sensitive("https://t.me/+SecretHash123"));
        assert!(contains_sensitive("https://signal.group/#CjQKI..."));
        assert!(contains_sensitive("tinyclient://join/abc123"));
        assert!(contains_sensitive("Your verification code is 482910"));
        assert!(contains_sensitive("Login code: 739-201"));

        assert!(!contains_sensitive(""));
        assert!(!contains_sensitive("See you at lunch around 12:30!"));
        assert!(!contains_sensitive("Check https://example.com/docs/guide"));
    }

    #[test]
    fn broadcast_process_matching_handles_obs_and_streamlabs() {
        assert!(is_broadcast_process("obs64.exe"));
        assert!(is_broadcast_process("OBS32.EXE"));
        assert!(is_broadcast_process("obs"));
        assert!(is_broadcast_process("/Applications/OBS.app/Contents/MacOS/obs"));
        assert!(is_broadcast_process("Streamlabs Desktop.exe"));
        assert!(is_broadcast_process("Streamlabs OBS.exe"));
        assert!(is_broadcast_process("streamlabs-desktop"));

        assert!(!is_broadcast_process(""));
        assert!(!is_broadcast_process("chrome.exe"));
        assert!(!is_broadcast_process("obsidian.exe"));
        assert!(!is_broadcast_process("obs-helper.exe"));
    }
}
