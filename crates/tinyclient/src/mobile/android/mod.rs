//! The Android phone: mediad's call platform and chatd's web sign-in, each a thin Rust layer over
//! a small Java class in the APK (`mobile/android/app/src/main/java/dev/tinyclient/app`):
//!
//! | Rust | Java |
//! |---|---|
//! | permissions, Settings | `TinyPlatform` (runtime requests through `TinyActivity`) |
//! | audio session, speaker/earpiece/Bluetooth | `CallAudio` + `CallService` (foreground, notification) |
//! | camera | `CameraCapture` (Camera2, NV12, upright) |
//! | screen share | `ScreenCapture` (MediaProjection consent, RGBA) |
//! | H.264 | `AvcEncoder`, `AvcDecoder` (MediaCodec) |
//! | Discord, Slack, Teams | `WebSession` + `WebSignInActivity` (WebView) |
//! | a Tiny server | `AuthSession` (Custom Tab; chatd's loopback listener receives the redirect) |
//!
//! It also routes the call screen's hooks (`gui::call::phone::hooks`): Open Settings, the audio
//! route menu and the camera flip. Threads and blocking follow docs/mobile-platform.md: every
//! method here may block its (mediad or chatd) caller; Java hops to the UI thread itself.

mod codec;
mod jvm;
#[cfg(feature = "demo")]
mod loopback;
mod natives;
mod selfie;
mod web;

use crate::gui::call::phone::hooks;
use jni::objects::JValue;
use jvm::{AUDIO, CAMERA, PLATFORM, SCREEN};
use mediad::platform::{CameraRequest, EncoderConfig, Facing, FrameSink, H264Decoder, H264Encoder, Permission, Platform};
use natives::{next_id, SINKS};
use protocol::web_sign_in::{AuthHandle, AuthRequest, WebPage, WebPageHandle, WebSignIn};
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;

/// Registers the Android platform and web sign-in, and routes the call screen's hooks.
pub fn register() {
    let phone = Arc::new(Android);
    mediad::platform::register(phone.clone());
    protocol::web_sign_in::register(phone);
    hooks::route_system_settings(|_| open_settings());
    hooks::route_audio(routes, choose_route);
    hooks::route_flip(flip);
    hooks::route_self_frame(selfie::shown);
    #[cfg(feature = "demo")]
    loopback::register();
}

struct Android;

/// The route the user chose in this call (`Route` as `CallAudio`'s number + 1), 0 for none.
static USER_ROUTE: AtomicU8 = AtomicU8::new(0);
/// The camera side the user flipped to in this call: 0 not flipped, 1 front, 2 back.
static FACING: AtomicU8 = AtomicU8::new(0);
/// The sinks of the running camera and screen share, 0 for none.
static CAMERA_SINK: AtomicI64 = AtomicI64::new(0);
static SCREEN_SINK: AtomicI64 = AtomicI64::new(0);

const EARPIECE: i32 = 0;
const SPEAKER: i32 = 1;
const BLUETOOTH: i32 = 2;
const WIRED: i32 = 3;

fn route_number(route: hooks::Route) -> i32 {
    match route {
        hooks::Route::Earpiece => EARPIECE,
        hooks::Route::Speaker => SPEAKER,
        hooks::Route::Bluetooth => BLUETOOTH,
        hooks::Route::Wired => WIRED,
    }
}

fn open_settings() {
    std::thread::spawn(|| {
        let _ = jvm::call("open Settings", |env| jvm::call_static(env, PLATFORM, "openSettings", "()V", &[]).map(|_| ()));
    });
}

fn routes() -> Vec<hooks::Route> {
    let mask = jvm::call("list the audio routes", |env| jvm::call_static(env, AUDIO, "routes", "()I", &[])?.i()).unwrap_or((1 << EARPIECE) | (1 << SPEAKER));
    [(EARPIECE, hooks::Route::Earpiece), (SPEAKER, hooks::Route::Speaker), (BLUETOOTH, hooks::Route::Bluetooth), (WIRED, hooks::Route::Wired)]
        .into_iter()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, route)| route)
        .collect()
}

/// Moves the call's sound; off the UI thread, since Android may take a moment.
fn choose_route(route: hooks::Route) {
    let number = route_number(route);
    USER_ROUTE.store(number as u8 + 1, Ordering::Relaxed);
    std::thread::spawn(move || set_route(number));
}

fn set_route(number: i32) -> Result<(), String> {
    let words = jvm::call("switch the audio route", |env| {
        let out = jvm::call_static(env, AUDIO, "setRoute", "(I)Ljava/lang/String;", &[JValue::Int(number)])?.l()?;
        jvm::string(env, out)
    })?;
    words.map_or(Ok(()), Err)
}

fn flip(front: bool) {
    FACING.store(if front { 1 } else { 2 }, Ordering::Relaxed);
    selfie::mirror(front);
    std::thread::spawn(move || {
        let words = jvm::call("turn the camera", |env| {
            let out = jvm::call_static(env, CAMERA, "flip", "(Z)Ljava/lang/String;", &[JValue::Bool(front.into())])?.l()?;
            jvm::string(env, out)
        });
        if let Ok(Some(words)) | Err(words) = words {
            log::warn!("tinyclient: the camera did not turn: {words}");
        }
    });
}

/// A Java method answering null for done or a sentence for what went wrong.
fn sentence(what: &str, class: &'static str, method: &str, sig: &str, args: &[JValue]) -> Result<(), String> {
    let words = jvm::call(what, |env| {
        let out = jvm::call_static(env, class, method, sig, args)?.l()?;
        jvm::string(env, out)
    })?;
    words.map_or(Ok(()), Err)
}

fn refusal(permission: Permission, code: i32) -> String {
    let (thing, why) = match permission {
        Permission::Microphone => ("microphone", "for calls"),
        Permission::Camera => ("camera", "to send video"),
    };
    match code {
        2 => format!("Tiny isn't allowed to use the {thing}. Turn it on in Settings to use it {why}."),
        3 => format!("Tiny can't ask for the {thing} right now. Open the app and try again."),
        _ => format!("Tiny needs the {thing} {why}."),
    }
}

impl Platform for Android {
    fn request_permission(&self, permission: Permission) -> Result<(), String> {
        let kind = match permission {
            Permission::Microphone => 0,
            Permission::Camera => 1,
        };
        let code = jvm::call("ask for the permission", |env| jvm::call_static(env, PLATFORM, "requestPermission", "(I)I", &[JValue::Int(kind)])?.i())?;
        let gui = match permission {
            Permission::Microphone => hooks::Permission::Microphone,
            Permission::Camera => hooks::Permission::Camera,
        };
        hooks::note_permission(gui, code != 0);
        log::info!("tinyclient: {permission:?} permission answered {code}");
        if code == 0 {
            Ok(())
        } else {
            Err(refusal(permission, code))
        }
    }

    fn start_audio_session(&self) -> Result<(), String> {
        let route = match USER_ROUTE.load(Ordering::Relaxed) {
            0 => EARPIECE,
            chosen => chosen as i32 - 1,
        };
        sentence("start the call's audio", AUDIO, "start", "(I)Ljava/lang/String;", &[JValue::Int(route)])
    }

    fn stop_audio_session(&self) {
        let _ = jvm::call("end the call's audio", |env| jvm::call_static(env, AUDIO, "stop", "()V", &[]).map(|_| ()));
        USER_ROUTE.store(0, Ordering::Relaxed);
        FACING.store(0, Ordering::Relaxed);
    }

    fn set_speaker(&self, on: bool) -> Result<(), String> {
        set_route(if on { SPEAKER } else { EARPIECE })
    }

    fn start_camera(&self, request: CameraRequest, sink: FrameSink) -> Result<(), String> {
        let front = match FACING.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => request.facing == Facing::Front,
        };
        let id = next_id();
        SINKS.put(id, selfie::tee(sink, front));
        let old = CAMERA_SINK.swap(id, Ordering::Relaxed);
        let started = sentence(
            "start the camera",
            CAMERA,
            "start",
            "(ZIIIJ)Ljava/lang/String;",
            &[JValue::Bool(front.into()), JValue::Int(request.width as i32), JValue::Int(request.height as i32), JValue::Int(request.fps as i32), JValue::Long(id)],
        );
        SINKS.take(old);
        if started.is_err() {
            SINKS.take(id);
            selfie::stop();
            let _ = CAMERA_SINK.compare_exchange(id, 0, Ordering::Relaxed, Ordering::Relaxed);
        } else if USER_ROUTE.load(Ordering::Relaxed) == 0 {
            // A video call plays on the speaker unless the user chose otherwise.
            let _ = set_route(SPEAKER);
        }
        started
    }

    fn stop_camera(&self) {
        let _ = jvm::call("stop the camera", |env| jvm::call_static(env, CAMERA, "stop", "()V", &[]).map(|_| ()));
        SINKS.take(CAMERA_SINK.swap(0, Ordering::Relaxed));
        selfie::stop();
    }

    fn start_screen_share(&self, sink: FrameSink) -> Result<(), String> {
        let id = next_id();
        SINKS.put(id, sink);
        let old = SCREEN_SINK.swap(id, Ordering::Relaxed);
        let started = sentence("share the screen", SCREEN, "start", "(J)Ljava/lang/String;", &[JValue::Long(id)]);
        SINKS.take(old);
        if started.is_err() {
            SINKS.take(id);
            let _ = SCREEN_SINK.compare_exchange(id, 0, Ordering::Relaxed, Ordering::Relaxed);
        }
        started
    }

    fn stop_screen_share(&self) {
        let _ = jvm::call("stop sharing the screen", |env| jvm::call_static(env, SCREEN, "stop", "()V", &[]).map(|_| ()));
        SINKS.take(SCREEN_SINK.swap(0, Ordering::Relaxed));
    }

    fn h264_encoder(&self, config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String> {
        codec::encoder(config)
    }

    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String> {
        codec::decoder()
    }
}

impl WebSignIn for Android {
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
        web::open_page(page)
    }

    fn open_auth(&self, auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
        web::open_auth(auth)
    }
}
