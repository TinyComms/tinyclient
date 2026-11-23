//! Android platform location provider bridging `protocol::location::LocationProvider` to
//! `dev.tinyclient.app.LocationBridge` and `LocationService` (#452).

use super::jvm;
use jni::objects::JValue;
use protocol::location::{GeoPoint, LocationProvider};

const LOCATION_BRIDGE: &str = "dev.tinyclient.app.LocationBridge";

pub(super) struct AndroidLocation;

impl LocationProvider for AndroidLocation {
    fn current_location(&self) -> Result<GeoPoint, String> {
        let raw = jvm::call("read device location", |env| {
            let out = jvm::call_static(env, LOCATION_BRIDGE, "currentLocation", "()Ljava/lang/String;", &[])?.l()?;
            jvm::string(env, out)
        })?
        .ok_or_else(|| "Location services returned no coordinates.".to_owned())?;
        if let Some(reason) = raw.strip_prefix("err:") {
            return Err(reason.to_owned());
        }
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() < 2 {
            return Err("Location services returned an unreadable coordinate.".to_owned());
        }
        let lat: f64 = parts[0].trim().parse().map_err(|_| "Invalid latitude from device.".to_owned())?;
        let lon: f64 = parts[1].trim().parse().map_err(|_| "Invalid longitude from device.".to_owned())?;
        let accuracy = parts.get(2).and_then(|s| s.trim().parse::<u32>().ok()).filter(|&a| a > 0);
        let heading = parts.get(3).and_then(|s| s.trim().parse::<u16>().ok()).filter(|&h| (1..=360).contains(&h));
        Ok(GeoPoint::from_deg(lat, lon)?.with_accuracy(accuracy).with_heading(heading))
    }

    fn start_live_tracking(&self, _active_chats: usize, summary: &str) -> Result<(), String> {
        let text = summary.to_owned();
        let err = jvm::call("start live location notification", |env| {
            let arg = env.new_string(&text)?;
            let out = jvm::call_static(
                env,
                LOCATION_BRIDGE,
                "startLiveTracking",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&arg)],
            )?
            .l()?;
            jvm::string(env, out)
        })?;
        err.map_or(Ok(()), Err)
    }

    fn stop_live_tracking(&self) {
        let _ = jvm::call("stop live location notification", |env| {
            jvm::call_static(env, LOCATION_BRIDGE, "stopLiveTracking", "()V", &[]).map(|_| ())
        });
    }
}
