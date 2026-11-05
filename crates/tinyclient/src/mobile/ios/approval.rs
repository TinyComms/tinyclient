use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAErrorDomain, LAPolicy};
use protocol::device_approval::{DeviceApproval, Outcome};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(super) struct IosApproval;

impl DeviceApproval for IosApproval {
    fn verify(&self, reason: &str, cancelled: &dyn Fn() -> bool) -> Result<Outcome, String> {
        if cancelled() {
            return Err("Device approval was cancelled.".to_owned());
        }
        let reason = reason.to_owned();
        let (send, receive) = mpsc::channel();
        let context = super::on_main(move |mtm| {
            if super::auth::key_window(mtm).is_none() {
                return Err("There is no active window for device approval.".to_owned());
            }
            // SAFETY: one fresh LAContext for this operation. All its method calls are routed
            // through the main queue; only its strong reference crosses threads in Shared.
            let context = unsafe { LAContext::new() };
            match unsafe { context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication) } {
                Ok(()) => {}
                Err(error)
                    if error.code() == LAError::PasscodeNotSet.0
                        && error.domain().to_string() == unsafe { LAErrorDomain }.to_string() =>
                {
                    let _ = send.send(Ok(Outcome::NotConfigured));
                    return Ok(None);
                }
                Err(_) => return Err("The phone could not start owner authentication.".to_owned()),
            }
            let reply = RcBlock::new(move |success: Bool, _: *mut NSError| {
                let _ = send.send(if success.as_bool() {
                    Ok(Outcome::Verified)
                } else {
                    Err("The phone did not approve adding this device.".to_owned())
                });
            });
            unsafe {
                context.evaluatePolicy_localizedReason_reply(
                    LAPolicy::DeviceOwnerAuthentication,
                    &NSString::from_str(&reason),
                    &reply,
                );
            }
            // LocalAuthentication copies the reply block; hold the context until completion.
            Ok(Some(super::Shared(context)))
        })?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let result = loop {
            if cancelled() || Instant::now() >= deadline {
                break Err("Device approval was cancelled.".to_owned());
            }
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break Err("The phone could not finish owner authentication.".to_owned()),
            }
        };
        if let Some(context) = context {
            super::on_main(move |_| unsafe { context.get().invalidate() });
        }
        result
    }
}
