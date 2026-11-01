//! What a phone call asks of the platform it runs on, and what the platform tells it.
//!
//! The call screen calls these; the mobile layer (`crate::mobile`) routes them to iOS and
//! Android by registering a function once at start (`route_*`). Until it does, each one does
//! what a desktop can: open the system's privacy settings, keep the audio route as chosen here,
//! and say nothing was refused. Nothing here blocks the UI thread.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

/// A permission a call needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Permission {
    Microphone,
    Camera,
}

/// Where the call's sound plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The earpiece: the phone held to the ear.
    Earpiece,
    Speaker,
    /// A Bluetooth headset or car.
    Bluetooth,
    /// Wired headphones. Only the platform reports them.
    #[allow(dead_code)]
    Wired,
}

impl Route {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Route::Earpiece => "Phone",
            Route::Speaker => "Speaker",
            Route::Bluetooth => "Bluetooth",
            Route::Wired => "Headphones",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Route::Earpiece => "icons/smartphone.svg",
            Route::Speaker => "icons/volume-2.svg",
            Route::Bluetooth => "icons/bluetooth.svg",
            Route::Wired => "icons/headphones.svg",
        }
    }
}

/// The platform's functions, each registered once.
struct Hooks {
    settings: OnceLock<fn(Permission)>,
    routes: OnceLock<fn() -> Vec<Route>>,
    route: OnceLock<fn(Route)>,
    flip: OnceLock<fn(bool)>,
}

static HOOKS: Hooks = Hooks { settings: OnceLock::new(), routes: OnceLock::new(), route: OnceLock::new(), flip: OnceLock::new() };

/// Refused permissions, as the platform last said: bit 0 the microphone, bit 1 the camera.
static DENIED: AtomicU8 = AtomicU8::new(0);
/// The route chosen here, for a platform that has not said how to change it.
static CHOSEN: Mutex<Option<Route>> = Mutex::new(None);

fn bit(kind: Permission) -> u8 {
    match kind {
        Permission::Microphone => 1,
        Permission::Camera => 2,
    }
}

/// The mobile layer: opens this app's page of the system settings, where `kind` is allowed.
#[allow(dead_code)] // Registered by the mobile layer.
pub(crate) fn route_system_settings(hook: fn(Permission)) {
    let _ = HOOKS.settings.set(hook);
}

/// The mobile layer: lists the routes the call can play through now, and moves it to one.
#[allow(dead_code)] // Registered by the mobile layer.
pub(crate) fn route_audio(routes: fn() -> Vec<Route>, choose: fn(Route)) {
    let _ = HOOKS.routes.set(routes);
    let _ = HOOKS.route.set(choose);
}

/// The mobile layer: turns the camera to the front (`true`) or the back.
#[allow(dead_code)] // Registered by the mobile layer.
pub(crate) fn route_flip(flip: fn(bool)) {
    let _ = HOOKS.flip.set(flip);
}

/// The platform says whether `kind` is refused. A refused microphone fails a call before it is
/// asked for (said with the way to the settings); a refused camera turns the camera button into
/// the way to them.
#[allow(dead_code)] // Called by the mobile layer.
pub(crate) fn note_permission(kind: Permission, denied: bool) {
    if denied {
        DENIED.fetch_or(bit(kind), Ordering::Relaxed);
    } else {
        DENIED.fetch_and(!bit(kind), Ordering::Relaxed);
    }
}

/// `kind` was refused, as far as anyone has said.
pub(crate) fn denied(kind: Permission) -> bool {
    #[cfg(feature = "gui-shot")]
    if super::super::shot::phone_denied(kind == Permission::Camera) {
        return true;
    }
    DENIED.load(Ordering::Relaxed) & bit(kind) != 0
}

/// Opens the system settings where `kind` is allowed for this app. The mobile layer routes it
/// (`route_system_settings`); a desktop opens its own privacy page.
pub(crate) fn open_system_settings(kind: Permission) {
    if let Some(hook) = HOOKS.settings.get() {
        hook(kind);
        return;
    }
    let url = match (kind, cfg!(target_os = "macos"), cfg!(windows)) {
        (Permission::Microphone, true, _) => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        (Permission::Camera, true, _) => "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera",
        (Permission::Microphone, _, true) => "ms-settings:privacy-microphone",
        (Permission::Camera, _, true) => "ms-settings:privacy-webcam",
        _ => return,
    };
    std::thread::spawn(move || {
        let _ = super::super::open(url);
    });
}

/// The routes the call can play through now. Without a platform: the phone's own two.
pub(crate) fn routes() -> Vec<Route> {
    #[cfg(feature = "gui-shot")]
    if crate::gui::shot_demo::on() {
        return vec![Route::Earpiece, Route::Speaker, Route::Bluetooth];
    }
    match HOOKS.routes.get() {
        Some(list) => list(),
        None => vec![Route::Earpiece, Route::Speaker],
    }
}

/// The route in use: what was chosen here, else `fallback` (a video call starts on the
/// speaker, a voice call at the ear).
pub(crate) fn route(fallback: Route) -> Route {
    CHOSEN.lock().ok().and_then(|held| *held).unwrap_or(fallback)
}

/// Moves the call's sound to `route`.
pub(crate) fn choose_route(route: Route) {
    if let Ok(mut held) = CHOSEN.lock() {
        *held = Some(route);
    }
    if let Some(hook) = HOOKS.route.get() {
        hook(route);
    }
}

/// Forgets the route chosen for the call that ended.
pub(crate) fn forget_route() {
    if let Ok(mut held) = CHOSEN.lock() {
        *held = None;
    }
}

/// Turns the camera to the front or the back.
pub(crate) fn flip_camera(front: bool) {
    if let Some(hook) = HOOKS.flip.get() {
        hook(front);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_kept_per_permission_until_lifted() {
        note_permission(Permission::Camera, true);
        assert!(DENIED.load(Ordering::Relaxed) & 2 != 0);
        assert!(DENIED.load(Ordering::Relaxed) & 1 == 0);
        note_permission(Permission::Camera, false);
        assert!(DENIED.load(Ordering::Relaxed) & 2 == 0);
    }

    #[test]
    fn every_route_names_itself_and_has_a_glyph() {
        for route in [Route::Earpiece, Route::Speaker, Route::Bluetooth, Route::Wired] {
            assert!(!route.label().is_empty());
            assert!(route.icon().starts_with("icons/"));
        }
    }
}
