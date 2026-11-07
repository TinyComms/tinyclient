//! Native approval belongs to the core, so direct IPC cannot bypass it.
use protocol::device_approval::{DeviceApproval, Outcome};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) fn service() -> Arc<dyn DeviceApproval> {
    protocol::device_approval::provider().unwrap_or_else(|| Arc::new(Desktop))
}

pub(super) fn verify(
    service: &dyn DeviceApproval,
    reason: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    match service.verify(reason, cancelled)? {
        Outcome::Verified | Outcome::NotConfigured | Outcome::NotPresent => Ok(()),
        Outcome::Unsupported if cfg!(target_os = "linux") => Ok(()),
        Outcome::Unsupported => Err(unavailable()),
    }
}

/// Legacy paste-code approval has the same owner-verification requirement. Dispatch here
/// before taking the device lock, so native UI never holds that lock while it waits.
pub(super) fn approve(saved: &super::Saved, code: &str) -> protocol::Response {
    let result = (|| {
        if super::parse_link_code(code).is_none() {
            return Err(
                "That isn't a device link code. Copy it again from the new device.".to_owned(),
            );
        }
        let shared = super::device(saved);
        {
            let mut device = super::lock(&shared);
            device.enrol_if_needed();
            if !device.ready() {
                return Err(
                    "This device has to be set up before it can approve another.".to_owned(),
                );
            }
        }
        // Legacy approval is a synchronous IPC request with a 45-second client timeout.
        let deadline = Instant::now() + Duration::from_secs(30);
        let cancelled = || Instant::now() >= deadline;
        verify(
            service().as_ref(),
            "Approve adding a device to your Tiny account",
            &cancelled,
        )?;
        if cancelled() {
            return Err("Device approval timed out.".to_owned());
        }
        publish_after_approval(&shared, code, &cancelled)
    })();
    match result {
        Ok(()) => protocol::Response::Ok,
        Err(detail) => protocol::Response::Error {
            code: protocol::ErrorCode::Refused,
            detail,
        },
    }
}

fn publish_after_approval(
    shared: &super::Shared,
    code: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    super::lock(shared).approve(code, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_approval_expiring_while_waiting_for_the_device_never_publishes() {
        refuses_expired_approval(0);
    }
    #[test]
    fn legacy_approval_expiring_after_signing_never_publishes() {
        refuses_expired_approval(1);
    }
    fn refuses_expired_approval(expire_after: usize) {
        let saved = super::super::Saved {
            base_url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            user_id: "11111111-1111-1111-1111-111111111111".into(),
            email: None,
            display_name: String::new(),
            role: "member".into(),
            plain_http_ok: false,
            e2ee: None,
        };
        let mut device = super::super::Device::new(&saved);
        device.dir =
            std::env::temp_dir().join(format!("tiny-legacy-approval-test-{}", std::process::id()));
        super::super::private_dir(&device.dir).unwrap();
        let user = e2ee::UserId::parse(&saved.user_id).unwrap();
        device.engine = Some(e2ee::Engine::detached(user).unwrap());
        device.state = protocol::DeviceState::Ready;
        let other = e2ee::Engine::detached(user).unwrap();
        let code = super::super::link_code(other.device_id(), &other.signature_key());
        let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = puts.clone();
        device.saved.base_url = super::super::super::tests::serve(move |request| {
            if request.starts_with("PUT ") {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            super::super::super::tests::http(200, "Test", "[]")
        });
        let shared = Arc::new(std::sync::Mutex::new(device));
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let result = publish_after_approval(&shared, &code, &|| {
            checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= expire_after
        });
        assert!(
            result.is_err(),
            "a deadline that elapsed while acquiring the lock must still be enforced"
        );
        assert_eq!(puts.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

struct Desktop;

impl DeviceApproval for Desktop {
    fn verify(&self, reason: &str, cancelled: &dyn Fn() -> bool) -> Result<Outcome, String> {
        // Phones cannot spawn helpers. A missing mobile registration is an error, never absence.
        if cfg!(any(target_os = "ios", target_os = "android")) {
            return Err("This phone could not ask the operating system for approval.".to_owned());
        }
        if cancelled() {
            return Err("Device approval was cancelled.".to_owned());
        }
        let path = crate::server::login_path().map_err(|_| unavailable())?;
        let mut command = Command::new(path);
        protocol::launch::no_console(&mut command)
            .args(["--device-approval", reason])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| unavailable())?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let result = (|| {
            loop {
                if cancelled() || Instant::now() >= deadline {
                    return Err("Device approval was cancelled or timed out.".to_owned());
                }
                if let Some(status) = child.try_wait().map_err(|_| unavailable())? {
                    if status.code() == Some(protocol::device_approval::API_UNAVAILABLE_EXIT) {
                        return Err("This Windows version cannot show the required device approval. Approve from another device, or use Windows 11 or later.".to_owned());
                    }
                    if !status.success() {
                        return Err(
                            "The operating system did not approve adding this device.".to_owned()
                        );
                    }
                    let mut output = Vec::new();
                    child
                        .stdout
                        .take()
                        .ok_or_else(unavailable)?
                        .take(1025)
                        .read_to_end(&mut output)
                        .map_err(|_| unavailable())?;
                    if output.len() > 1024 {
                        return Err(unavailable());
                    }
                    let outcome: Outcome =
                        serde_json::from_slice(&output).map_err(|_| unavailable())?;
                    // Unsupported is an explicit Linux fallback; never trust that response on
                    // platforms for which a native verifier is required.
                    if outcome == Outcome::Unsupported && !cfg!(target_os = "linux") {
                        return Err(unavailable());
                    }
                    return Ok(outcome);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })();
        if result.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        result
    }
}

fn unavailable() -> String {
    "This device could not ask the operating system for approval. Quit and reopen Tiny, then try again.".to_owned()
}
