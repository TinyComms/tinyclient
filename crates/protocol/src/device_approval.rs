//! Local device-owner approval, called by the core before it signs another device.
//! A phone registers its native provider before the core starts. No IPC request may assert
//! that authentication succeeded; only the native provider can return this result.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// The native helper found a configured verifier but its desktop API is not implemented.
/// This is a refusal, not the native-unavailable fallback.
pub const API_UNAVAILABLE_EXIT: i32 = 78;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Verified,
    /// A fresh native check found no configured verifier.
    NotConfigured,
    /// A fresh native check found no verifier device.
    NotPresent,
    /// This operating system has no supported native reauthentication integration.
    Unsupported,
}

/// Runs on a core worker, never the UI thread. Implementations must dismiss pending UI when
/// `cancelled` becomes true and must not reinterpret cancellation or native errors as absence.
pub trait DeviceApproval: Send + Sync {
    fn verify(&self, reason: &str, cancelled: &dyn Fn() -> bool) -> Result<Outcome, String>;
}

impl<F> DeviceApproval for F
where
    F: Fn(&str, &dyn Fn() -> bool) -> Result<Outcome, String> + Send + Sync,
{
    fn verify(&self, reason: &str, cancelled: &dyn Fn() -> bool) -> Result<Outcome, String> {
        self(reason, cancelled)
    }
}

static PROVIDER: RwLock<Option<Arc<dyn DeviceApproval>>> = RwLock::new(None);

pub fn register(provider: Arc<dyn DeviceApproval>) {
    *PROVIDER.write().unwrap_or_else(|p| p.into_inner()) = Some(provider);
}

pub fn provider() -> Option<Arc<dyn DeviceApproval>> {
    PROVIDER.read().unwrap_or_else(|p| p.into_inner()).clone()
}
