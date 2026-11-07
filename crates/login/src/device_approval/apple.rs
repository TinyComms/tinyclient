use super::{finish, Outcome};
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAErrorDomain, LAPolicy};

pub(super) fn run(reason: &str) {
    let mtm = MainThreadMarker::new().expect("approval helper main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    // SAFETY: a fresh context is used for one operation, on this helper's main thread.
    let context = unsafe { LAContext::new() };
    match unsafe { context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication) } {
        Ok(()) => {}
        Err(error)
            if error.code() == LAError::PasscodeNotSet.0
                && error.domain().to_string() == unsafe { LAErrorDomain }.to_string() =>
        {
            finish(Ok(Outcome::NotConfigured))
        }
        Err(_) => finish(Err("Owner authentication cannot start.".to_owned())),
    }
    let reply = RcBlock::new(|success: Bool, _: *mut NSError| {
        finish(if success.as_bool() {
            Ok(Outcome::Verified)
        } else {
            Err("Owner authentication was refused.".to_owned())
        });
    });
    unsafe {
        context.evaluatePolicy_localizedReason_reply(
            LAPolicy::DeviceOwnerAuthentication,
            &NSString::from_str(reason),
            &reply,
        );
    }
    // Keep the context and block alive while AppKit pumps the authentication UI.
    let _keep = (context, reply);
    app.run();
}
