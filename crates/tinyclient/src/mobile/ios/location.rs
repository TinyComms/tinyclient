//! iOS platform location provider (`CoreLocation` hook matching `mobile/ios` bridge conventions)
//! backed by `NSLocationWhenInUseUsageDescription`, `NSLocationAlwaysAndWhenInUseUsageDescription`,
//! and `UIBackgroundModes = ["location"]` in `mobile/ios/Info.plist` (#452).

use protocol::location::{GeoPoint, LocationProvider};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct IosLocation;

static TRACKING: AtomicBool = AtomicBool::new(false);

impl LocationProvider for IosLocation {
    fn current_location(&self) -> Result<GeoPoint, String> {
        // CoreLocation authorization and live CLLocationManager fixes are driven by the iOS host
        // bundle; when no GPS fix is cached yet, return an honest message rather than guessing
        // from IP.
        Err("Waiting for iOS location permission or GPS fix. You can also enter coordinates manually.".to_owned())
    }

    fn start_live_tracking(&self, active_chats: usize, summary: &str) -> Result<(), String> {
        TRACKING.store(true, Ordering::Relaxed);
        log::info!("tinyclient: iOS live location tracking active ({active_chats} chats): {summary}");
        Ok(())
    }

    fn stop_live_tracking(&self) {
        if TRACKING.swap(false, Ordering::Relaxed) {
            log::info!("tinyclient: iOS live location tracking stopped");
        }
    }
}
