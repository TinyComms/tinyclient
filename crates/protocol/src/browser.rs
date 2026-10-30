//! The browser the sign-in window presents, and that chatd goes on being for that session. One
//! place, so the window and chatd cannot drift apart, and read from what is installed, so it does
//! not age: Slack turns away a Chrome older than its last few releases ("your browser is not
//! supported"), and a fixed version number gets there within a year.
//!
//! macOS: the installed Safari, which is the engine the window runs. Windows: the installed
//! WebView2, the Chromium the window runs, so the version agrees with the client hints it sends.
//! Linux: WebKitGTK is not Chromium and has no Chrome version to read, so the Chrome of the day,
//! counted from Chrome's four-week release schedule.

#[cfg(target_os = "macos")]
pub fn user_agent() -> String {
    let installed = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "/Applications/Safari.app/Contents/Info", "CFBundleShortVersionString"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.'));
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
         (KHTML, like Gecko) Version/{} Safari/605.1.15",
        installed.as_deref().unwrap_or("26.0")
    )
}

#[cfg(target_os = "windows")]
pub fn user_agent() -> String {
    let chrome = webview2_version().as_deref().and_then(major).unwrap_or_else(|| estimated_chrome(now()));
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{chrome}.0.0.0 Safari/537.36"
    )
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{}.0.0.0 Safari/537.36",
        estimated_chrome(now())
    )
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Chrome 137 reached stable on 2025-05-27, and a new major has followed every four weeks. A
/// skipped holiday release leaves this a version behind, never ahead: an old browser is let in,
/// one that does not exist yet stands out.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn estimated_chrome(unix_secs: u64) -> u32 {
    const CHROME_137: u64 = 1_748_304_000;
    137 + (unix_secs.saturating_sub(CHROME_137) / (28 * 86_400)) as u32
}

#[cfg_attr(not(windows), allow(dead_code))]
fn major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok().filter(|&major| major > 0)
}

/// The installed WebView2 Runtime's version, as Edge's updater records it: machine-wide (under
/// WOW6432Node on 64-bit Windows), or per user when it was installed without administrator.
#[cfg(windows)]
fn webview2_version() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };
    const CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let places: [(HKEY, String); 3] = [
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\{CLIENT}")),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{CLIENT}")),
        (HKEY_CURRENT_USER, format!(r"Software\{CLIENT}")),
    ];
    let value = wide("pv");
    places.iter().find_map(|(root, path)| {
        let path = wide(path);
        let mut text = [0u16; 64];
        let mut bytes = std::mem::size_of_val(&text) as u32;
        let status = unsafe {
            RegGetValueW(
                *root,
                path.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                text.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != 0 {
            return None;
        }
        let end = text.iter().position(|&unit| unit == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..end])).filter(|version| major(version).is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_estimate_moves_one_chrome_every_four_weeks() {
        const DAY: u64 = 86_400;
        let chrome_137 = 1_748_304_000;
        assert_eq!(estimated_chrome(chrome_137), 137);
        assert_eq!(estimated_chrome(chrome_137 + 27 * DAY), 137);
        assert_eq!(estimated_chrome(chrome_137 + 28 * DAY), 138);
        // 2026-10-27, when Chrome and WebView2 were at 156: a version behind, never ahead.
        assert_eq!(estimated_chrome(1_793_059_200), 155);
        // A clock set before the anchor still names a Chrome Slack lets in.
        assert_eq!(estimated_chrome(0), 137);
    }

    #[test]
    fn a_version_is_read_by_its_major() {
        assert_eq!(major("156.0.4314.50"), Some(156));
        assert_eq!(major("0.0.0.0"), None);
        assert_eq!(major(""), None);
        assert_eq!(major("beta"), None);
    }

    #[test]
    fn the_browser_is_never_older_than_slack_takes() {
        let agent = user_agent();
        if let Some((_, rest)) = agent.split_once("Chrome/") {
            assert!(major(rest).is_some_and(|chrome| chrome >= 137), "{agent}");
        }
    }
}
