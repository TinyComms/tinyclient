//! Location sharing and live location tracking (#452), plus the mobile platform location bridge.
//!
//! Coordinates are stored in fixed-point 1e-7 degrees (`i64`) so every protocol message stays
//! `Eq` and deterministic without floating-point NaN/equality edge cases.

use crate::Key;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Telegram's sentinel period for "Share until I turn it off" (`0x7fffffff`).
pub const LIVE_LOCATION_INDEFINITE: u32 = 0x7fff_ffff;

/// Standard live-location duration choices offered in the phone composer.
pub const LIVE_LOCATION_PRESETS: &[(u32, &str)] = &[
    (15 * 60, "15 minutes"),
    (60 * 60, "1 hour"),
    (8 * 60 * 60, "8 hours"),
    (LIVE_LOCATION_INDEFINITE, "Until I turn it off"),
];

/// A validated geographic coordinate pair with optional horizontal accuracy and heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GeoPoint {
    /// Latitude in 1e-7 degrees (e.g. `37.7749` -> `377_749_000`).
    pub lat_e7: i64,
    /// Longitude in 1e-7 degrees (e.g. `-122.4194` -> `-1_224_194_000`).
    pub lon_e7: i64,
    /// Horizontal accuracy radius in meters when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy_meters: Option<u32>,
    /// Compass heading in degrees (`1..=360`) when moving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<u16>,
}

impl GeoPoint {
    const SCALE: f64 = 10_000_000.0;

    /// Builds a validated `GeoPoint` from decimal degrees. Rejects NaN, infinity, and out-of-range
    /// coordinates.
    pub fn from_deg(lat: f64, lon: f64) -> Result<GeoPoint, String> {
        if !lat.is_finite() || !lon.is_finite() {
            return Err("Coordinates must be finite numbers.".to_owned());
        }
        if !(-90.0..=90.0).contains(&lat) {
            return Err("Latitude must be between -90 and 90.".to_owned());
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err("Longitude must be between -180 and 180.".to_owned());
        }
        Ok(GeoPoint {
            lat_e7: (lat * Self::SCALE).round() as i64,
            lon_e7: (lon * Self::SCALE).round() as i64,
            accuracy_meters: None,
            heading: None,
        })
    }

    pub fn with_accuracy(mut self, accuracy_meters: Option<u32>) -> Self {
        self.accuracy_meters = accuracy_meters.filter(|&m| m > 0);
        self
    }

    pub fn with_heading(mut self, heading: Option<u16>) -> Self {
        self.heading = heading.filter(|&h| (1..=360).contains(&h));
        self
    }

    pub fn lat(&self) -> f64 {
        self.lat_e7 as f64 / Self::SCALE
    }

    pub fn lon(&self) -> f64 {
        self.lon_e7 as f64 / Self::SCALE
    }

    pub fn is_valid(&self) -> bool {
        (-900_000_000..=900_000_000).contains(&self.lat_e7)
            && (-1_800_000_000..=1_800_000_000).contains(&self.lon_e7)
    }

    /// Parses honest user-typed coordinates (`lat, lon` or `geo:lat,lon`). Never guesses from IP.
    pub fn parse_typed(text: &str) -> Option<GeoPoint> {
        let trimmed = text.trim();
        let body = trimmed.strip_prefix("geo:").unwrap_or(trimmed).trim();
        let (lat_s, lon_s) = body.split_once(',')?;
        let lat: f64 = lat_s.trim().parse().ok()?;
        let lon: f64 = lon_s.trim().parse().ok()?;
        GeoPoint::from_deg(lat, lon).ok()
    }
}

/// One actively running live location share owned by this account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveLocationShare {
    pub conversation: Key,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conversation_title: String,
    pub message_id: String,
    pub point: GeoPoint,
    /// Unix seconds when live sharing began.
    pub started_at: i64,
    /// Requested duration in seconds (`LIVE_LOCATION_INDEFINITE` for until turned off).
    pub period_secs: u32,
    /// Unix seconds when Telegram stops updating automatically; `None` for indefinite shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Optional proximity alert radius in meters (direct chats only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proximity_alert_radius: Option<u32>,
    /// Unix seconds of the most recent coordinate edit.
    pub last_updated_at: i64,
}

impl LiveLocationShare {
    pub fn is_indefinite(&self) -> bool {
        self.period_secs == LIVE_LOCATION_INDEFINITE || self.expires_at.is_none()
    }

    pub fn is_active_at(&self, now_unix: i64) -> bool {
        match self.expires_at {
            Some(until) => until > now_unix,
            None => self.period_secs == LIVE_LOCATION_INDEFINITE,
        }
    }

    pub fn remaining_secs_at(&self, now_unix: i64) -> Option<i64> {
        self.expires_at.map(|until| (until - now_unix).max(0))
    }

    /// Formats a human-readable remaining duration label for the persistent sharing bar.
    pub fn status_label(&self, now_unix: i64) -> String {
        if self.is_indefinite() {
            return "Until turned off".to_owned();
        }
        let secs = self.remaining_secs_at(now_unix).unwrap_or(0);
        if secs <= 0 {
            return "Ending…".to_owned();
        }
        let mins = (secs + 59) / 60;
        if mins < 60 {
            format!("{mins}m left")
        } else {
            let hours = mins / 60;
            let rem = mins % 60;
            if rem == 0 {
                format!("{hours}h left")
            } else {
                format!("{hours}h {rem}m left")
            }
        }
    }
}

/// Validates a live-location duration choice. Telegram accepts 60..=86400 seconds or
/// `LIVE_LOCATION_INDEFINITE` (`0x7fffffff`).
pub fn valid_live_period(period_secs: u32) -> bool {
    period_secs == LIVE_LOCATION_INDEFINITE || (60..=86_400).contains(&period_secs)
}

/// Platform location bridge registered by Android and iOS phone shells (like `device_approval`).
/// Desktop leaves no provider registered and never synthesizes coordinates from IP.
pub trait LocationProvider: Send + Sync {
    /// Requests a fresh one-time GPS/system location fix (prompting for OS permission if needed).
    fn current_location(&self) -> Result<GeoPoint, String>;
    /// Starts or updates the platform's foreground/background location tracking notification while
    /// one or more live shares are active.
    fn start_live_tracking(&self, active_chats: usize, summary: &str) -> Result<(), String>;
    /// Stops platform live location tracking and dismisses any foreground service notification.
    fn stop_live_tracking(&self);
}

static PROVIDER: RwLock<Option<Arc<dyn LocationProvider>>> = RwLock::new(None);

pub fn register(provider: Arc<dyn LocationProvider>) {
    *PROVIDER.write().unwrap_or_else(|p| p.into_inner()) = Some(provider);
}

pub fn provider() -> Option<Arc<dyn LocationProvider>> {
    PROVIDER.read().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Network;

    #[test]
    fn geo_point_validates_and_parses_typed_coords() {
        let pt = GeoPoint::parse_typed("37.7749, -122.4194").expect("valid pair");
        assert!((pt.lat() - 37.7749).abs() < 1e-6);
        assert!((pt.lon() + 122.4194).abs() < 1e-6);
        assert!(pt.is_valid());

        let geo = GeoPoint::parse_typed("geo:51.5007,-0.1246").expect("valid geo uri");
        assert!((geo.lat() - 51.5007).abs() < 1e-6);

        assert!(GeoPoint::parse_typed("91.0, 0.0").is_none());
        assert!(GeoPoint::parse_typed("0.0, 181.0").is_none());
        assert!(GeoPoint::parse_typed("near downtown").is_none());
        assert!(GeoPoint::from_deg(f64::NAN, 0.0).is_err());
    }

    #[test]
    fn live_location_share_expiry_and_indefinite_status() {
        let key = Key {
            network: Network::Telegram,
            account: "7001".into(),
            id: "u7002".into(),
        };
        let timed = LiveLocationShare {
            conversation: key.clone(),
            conversation_title: "Nadia".into(),
            message_id: "42".into(),
            point: GeoPoint::from_deg(37.77, -122.42).unwrap().with_accuracy(Some(12)),
            started_at: 1_000,
            period_secs: 900,
            expires_at: Some(1_900),
            proximity_alert_radius: Some(250),
            last_updated_at: 1_000,
        };
        assert!(timed.is_active_at(1_500));
        assert!(!timed.is_active_at(1_900));
        assert_eq!(timed.status_label(1_300), "10m left");

        let indef = LiveLocationShare {
            expires_at: None,
            period_secs: LIVE_LOCATION_INDEFINITE,
            ..timed.clone()
        };
        assert!(indef.is_indefinite());
        assert!(indef.is_active_at(99_999_999));
        assert_eq!(indef.status_label(1_300), "Until turned off");

        assert!(valid_live_period(900));
        assert!(valid_live_period(3600));
        assert!(valid_live_period(28800));
        assert!(valid_live_period(LIVE_LOCATION_INDEFINITE));
        assert!(!valid_live_period(10));
        assert!(!valid_live_period(0));
    }
}
