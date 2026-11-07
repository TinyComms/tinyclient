//! Native owner verification for chatd. This mode opens no web page and receives no secrets.
use protocol::device_approval::Outcome;

fn finish(result: Result<Outcome, String>) -> ! {
    match result {
        Ok(outcome) => {
            println!(
                "{}",
                serde_json::to_string(&outcome).expect("approval outcome")
            );
            std::process::exit(0);
        }
        // Native error details can contain account information; the core supplies the words.
        Err(_) => std::process::exit(1),
    }
}

#[cfg(windows)]
#[path = "device_approval/windows.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "device_approval/apple.rs"]
mod native;

pub(super) fn run(reason: &str) {
    #[cfg(any(windows, target_os = "macos"))]
    native::run(reason);
    #[cfg(target_os = "linux")]
    {
        let _ = reason;
        // Linux has no supported device-owner reauthentication API in this client. The link's
        // code/digit confirmation still applies; this is not a claim of biometric verification.
        finish(Ok(Outcome::Unsupported));
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = reason;
        finish(Err("Unsupported approval host".to_owned()));
    }
}
