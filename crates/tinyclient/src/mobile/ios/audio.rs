//! Permissions, the call's audio session and its routes, and the app's page in Settings.
//!
//! `AVAudioSession` is thread-safe and is used from mediad's call thread directly. Permissions
//! ask `AVCaptureDevice` (the same answer `AVAudioApplication` gives for the microphone, and one
//! API for both) and wait on the caller's thread for the user.

use crate::gui::call::phone::hooks::{self, Route};
use block2::RcBlock;
use mediad::platform::Permission;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio, AVMediaTypeVideo};
use objc2_avf_audio::{
    AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayAndRecord, AVAudioSessionModeVoiceChat,
    AVAudioSessionPortBluetoothHFP, AVAudioSessionPortBuiltInMic, AVAudioSessionPortHeadphones,
    AVAudioSessionPortHeadsetMic, AVAudioSessionPortOverride, AVAudioSessionPortDescription,
    AVAudioSessionSetActiveOptions,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};
use objc2_ui_kit::{UIApplication, UIApplicationOpenSettingsURLString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The user moved the sound themselves during this call: a camera starting must not move it.
static USER_ROUTE: AtomicBool = AtomicBool::new(false);
/// A call's session is active.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The call screen's hooks: Settings, the routes and the route chosen.
pub(super) fn register_hooks() {
    hooks::route_system_settings(open_settings);
    hooks::route_audio(available_routes, choose_route);
    // What the system said before this launch, so a refused microphone shows its way to
    // Settings before a call is tried.
    for kind in [Permission::Microphone, Permission::Camera] {
        if status(kind) == AVAuthorizationStatus::Denied || status(kind) == AVAuthorizationStatus::Restricted {
            hooks::note_permission(hook_kind(kind), true);
        }
    }
}

fn hook_kind(kind: Permission) -> hooks::Permission {
    match kind {
        Permission::Microphone => hooks::Permission::Microphone,
        Permission::Camera => hooks::Permission::Camera,
    }
}

fn media_type(kind: Permission) -> &'static NSString {
    // SAFETY: AVFoundation's constants, always present on iOS.
    unsafe {
        match kind {
            Permission::Microphone => AVMediaTypeAudio.expect("AVMediaTypeAudio"),
            Permission::Camera => AVMediaTypeVideo.expect("AVMediaTypeVideo"),
        }
    }
}

fn status(kind: Permission) -> AVAuthorizationStatus {
    // SAFETY: a class method with a constant argument.
    unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type(kind)) }
}

fn refused(kind: Permission) -> String {
    match kind {
        Permission::Microphone => {
            "Tiny isn't allowed to use the microphone. Turn it on in Settings › Tiny › Microphone.".to_owned()
        }
        Permission::Camera => "Tiny isn't allowed to use the camera. Turn it on in Settings › Tiny › Camera.".to_owned(),
    }
}

/// Asks for `kind`, blocking until the user answers. Says the answer to the call screen too.
pub(super) fn request_permission(kind: Permission) -> Result<(), String> {
    let answer = match status(kind) {
        AVAuthorizationStatus::Authorized => true,
        AVAuthorizationStatus::NotDetermined => ask(kind),
        _ => false,
    };
    hooks::note_permission(hook_kind(kind), !answer);
    if answer {
        Ok(())
    } else {
        log::info!("tinyclient: {kind:?} permission refused");
        Err(refused(kind))
    }
}

/// Shows the system's prompt and waits for it. The completion runs on a queue of the system's.
fn ask(kind: Permission) -> bool {
    let (sender, answer) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = sender.send(granted.as_bool());
    });
    // The prompt must come from the main thread for UIKit to show it over the window.
    let block = super::Shared(block);
    super::on_main(move |_| {
        // SAFETY: the block lives as long as AVFoundation keeps it (it copies it).
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type(kind), &block.0) };
    });
    // A person may take a while; ten minutes is "never answered".
    answer.recv_timeout(Duration::from_secs(600)).unwrap_or(false)
}

fn session() -> Retained<AVAudioSession> {
    // SAFETY: the process-wide session.
    unsafe { AVAudioSession::sharedInstance() }
}

/// A call starts: play and record, voice chat (echo cancellation), Bluetooth headsets allowed.
pub(super) fn start_session() -> Result<(), String> {
    let session = session();
    let options = AVAudioSessionCategoryOptions::AllowBluetooth;
    // SAFETY: the constants are always present; the session is thread-safe.
    unsafe {
        session
            .setCategory_mode_options_error(
                AVAudioSessionCategoryPlayAndRecord.expect("category"),
                AVAudioSessionModeVoiceChat.expect("mode"),
                options,
            )
            .map_err(|error| format!("The call's audio could not be set up ({}).", error.localizedDescription()))?;
        session
            .setActive_withOptions_error(true, AVAudioSessionSetActiveOptions(0))
            .map_err(|error| format!("The call's audio could not start ({}).", error.localizedDescription()))?;
    }
    ACTIVE.store(true, Ordering::Relaxed);
    USER_ROUTE.store(false, Ordering::Relaxed);
    // A route chosen on the call screen before the session existed.
    if let Some(route) = CHOSEN.lock().ok().and_then(|held| *held) {
        apply_route(route);
    }
    log::info!("tinyclient: audio session active");
    Ok(())
}

/// The call ended: give the audio back to whatever played before.
pub(super) fn stop_session() {
    ACTIVE.store(false, Ordering::Relaxed);
    USER_ROUTE.store(false, Ordering::Relaxed);
    if let Ok(mut held) = CHOSEN.lock() {
        *held = None;
    }
    let session = session();
    // SAFETY: as in `start_session`.
    let done = unsafe {
        let _ = session.overrideOutputAudioPort_error(AVAudioSessionPortOverride::None);
        session.setActive_withOptions_error(false, AVAudioSessionSetActiveOptions::NotifyOthersOnDeactivation)
    };
    if let Err(error) = done {
        log::warn!("tinyclient: the audio session did not stop: {}", error.localizedDescription());
    }
}

/// The loudspeaker or the earpiece.
pub(super) fn set_speaker(on: bool) -> Result<(), String> {
    let port = if on { AVAudioSessionPortOverride::Speaker } else { AVAudioSessionPortOverride::None };
    // SAFETY: as in `start_session`.
    unsafe { session().overrideOutputAudioPort_error(port) }
        .map_err(|error| format!("The sound could not be moved ({}).", error.localizedDescription()))
}

/// A camera came on: a video call plays on the loudspeaker unless the user chose otherwise.
pub(super) fn video_started() {
    if ACTIVE.load(Ordering::Relaxed) && !USER_ROUTE.load(Ordering::Relaxed) && bluetooth_port().is_none() {
        let _ = set_speaker(true);
    }
}

/// The route the call screen chose, kept for a session that starts after the choice.
static CHOSEN: std::sync::Mutex<Option<Route>> = std::sync::Mutex::new(None);

fn inputs() -> Vec<Retained<AVAudioSessionPortDescription>> {
    // SAFETY: as in `start_session`.
    unsafe { session().availableInputs() }.map(|list| list.to_vec()).unwrap_or_default()
}

fn port_of(kind: &NSString) -> Option<Retained<AVAudioSessionPortDescription>> {
    // SAFETY: reading the port's type.
    inputs().into_iter().find(|port| unsafe { port.portType() }.isEqualToString(kind))
}

fn bluetooth_port() -> Option<Retained<AVAudioSessionPortDescription>> {
    // SAFETY: AVFAudio's constant.
    port_of(unsafe { AVAudioSessionPortBluetoothHFP }.expect("BluetoothHFP"))
}

fn headset_port() -> Option<Retained<AVAudioSessionPortDescription>> {
    // SAFETY: AVFAudio's constant.
    port_of(unsafe { AVAudioSessionPortHeadsetMic }.expect("HeadsetMic"))
}

/// Headphones without a microphone show only as an output of the current route.
fn headphones_out() -> bool {
    // SAFETY: reading the current route.
    unsafe {
        let headphones = AVAudioSessionPortHeadphones.expect("Headphones");
        session().currentRoute().outputs().iter().any(|port| port.portType().isEqualToString(headphones))
    }
}

/// The routes the call can play through now.
fn available_routes() -> Vec<Route> {
    let mut routes = Vec::new();
    if headset_port().is_some() || headphones_out() {
        routes.push(Route::Wired);
    } else {
        routes.push(Route::Earpiece);
    }
    routes.push(Route::Speaker);
    if bluetooth_port().is_some() {
        routes.push(Route::Bluetooth);
    }
    routes
}

/// The call screen chose `route`.
fn choose_route(route: Route) {
    USER_ROUTE.store(true, Ordering::Relaxed);
    if let Ok(mut held) = CHOSEN.lock() {
        *held = Some(route);
    }
    if ACTIVE.load(Ordering::Relaxed) {
        // Off the UI thread: AVAudioSession route changes can take a moment.
        std::thread::spawn(move || apply_route(route));
    }
}

fn apply_route(route: Route) {
    let session = session();
    let preferred = match route {
        Route::Speaker => None,
        // SAFETY: AVFAudio's constant.
        Route::Earpiece => port_of(unsafe { AVAudioSessionPortBuiltInMic }.expect("BuiltInMic")),
        Route::Bluetooth => bluetooth_port(),
        Route::Wired => headset_port(),
    };
    // SAFETY: as in `start_session`.
    let done = unsafe {
        if route == Route::Speaker {
            session.overrideOutputAudioPort_error(AVAudioSessionPortOverride::Speaker)
        } else {
            session
                .overrideOutputAudioPort_error(AVAudioSessionPortOverride::None)
                .and_then(|()| session.setPreferredInput_error(preferred.as_deref()))
        }
    };
    match done {
        Ok(()) => log::info!("tinyclient: audio route {route:?}"),
        Err(error) => log::warn!("tinyclient: audio route {route:?} failed: {}", error.localizedDescription()),
    }
}

/// Opens this app's page in Settings, where the microphone and camera are allowed.
fn open_settings(_kind: hooks::Permission) {
    super::on_main_later(|mtm| {
        let app = UIApplication::sharedApplication(mtm);
        // SAFETY: UIKit's constant names a valid URL; the options dictionary is empty.
        unsafe {
            let url = NSURL::URLWithString(UIApplicationOpenSettingsURLString);
            if let Some(url) = url {
                app.openURL_options_completionHandler(&url, &NSDictionary::new(), None);
            }
        }
    });
}
