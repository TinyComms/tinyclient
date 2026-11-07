use super::jvm;
use jni::objects::JValue;
use protocol::device_approval::{DeviceApproval, Outcome};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

const CLASS: &str = "dev.tinyclient.app.DeviceApproval";
pub(super) struct AndroidApproval;

impl DeviceApproval for AndroidApproval {
    fn verify(&self, reason: &str, cancelled: &dyn Fn() -> bool) -> Result<Outcome, String> {
        static NEXT: AtomicI64 = AtomicI64::new(1);
        if cancelled() {
            return Err("Device approval was cancelled.".to_owned());
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        jvm::call("ask for device approval", |env| {
            let reason = env.new_string(reason)?;
            jvm::call_static(
                env,
                CLASS,
                "start",
                "(JLjava/lang/String;)V",
                &[JValue::Long(id), JValue::Object(&reason)],
            )
            .map(|_| ())
        })?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let result = (|| loop {
            if cancelled() || Instant::now() >= deadline {
                return Err("Device approval was cancelled.".to_owned());
            }
            let status = jvm::call("read device approval", |env| {
                jvm::call_static(env, CLASS, "status", "(J)I", &[JValue::Long(id)])?.i()
            })?;
            match status {
                0 => std::thread::sleep(Duration::from_millis(100)),
                1 => return Ok(Outcome::Verified),
                2 => return Ok(Outcome::NotConfigured),
                _ => return Err("Android did not approve adding this device.".to_owned()),
            }
        })();
        let _ = jvm::call("close device approval", |env| {
            jvm::call_static(env, CLASS, "cancel", "(J)V", &[JValue::Long(id)]).map(|_| ())
        });
        result
    }
}
