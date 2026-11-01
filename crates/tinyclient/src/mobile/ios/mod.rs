//! The iOS phone: Apple's frameworks behind `mediad::platform::Platform` and
//! `protocol::web_sign_in::WebSignIn`, written through objc2.
//!
//! * `audio`: microphone and camera permission (`AVCaptureDevice`), the call's `AVAudioSession`,
//!   its routes (earpiece, speaker, Bluetooth, headphones) and the Settings page.
//! * `camera`: `AVCaptureSession` front or back, NV12 frames to the sink.
//! * `screen`: ReplayKit's in-app capture (`RPScreenRecorder`).
//! * `codec`: VideoToolbox H.264, Annex B in and out.
//! * `web`: Discord, Slack and Teams in a `WKWebView` presented over the GPUI window.
//! * `auth`: a Tiny server's OIDC page in `ASWebAuthenticationSession` (passkeys work there).
//!
//! mediad and chatd call these from their own threads. Everything UIKit, WebKit or
//! AuthenticationServices needs is done on the main thread through [`on_main`]; the waiting
//! (a permission answer, a script's result) happens on the caller's thread, never the main one.

mod audio;
mod auth;
mod camera;
mod codec;
mod pixels;
mod screen;
mod web;

use mediad::platform::{CameraRequest, EncoderConfig, FrameSink, H264Decoder, H264Encoder, Permission, Platform};
use objc2::MainThreadMarker;
use protocol::web_sign_in::{AuthHandle, AuthRequest, WebPage, WebPageHandle, WebSignIn};
use std::sync::Arc;

/// Registers the iOS phone with mediad, chatd's sign-in and the call screen.
pub(super) fn register() {
    let phone = Arc::new(Ios);
    mediad::platform::register(phone.clone());
    protocol::web_sign_in::register(phone);
    audio::register_hooks();
    camera::register_hooks();
}

/// Runs `work` on the main thread and returns its result. On the main thread already, runs it
/// at once.
pub(super) fn on_main<R: Send>(work: impl FnOnce(MainThreadMarker) -> R + Send) -> R {
    if let Some(mtm) = MainThreadMarker::new() {
        return work(mtm);
    }
    let mut out = None;
    let slot = &mut out;
    dispatch2::DispatchQueue::main().exec_sync(move || {
        // SAFETY: the main queue runs on the main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        *slot = Some(work(mtm));
    });
    out.expect("the main queue ran the work")
}

/// Runs `work` on the main thread later, without waiting.
pub(super) fn on_main_later(work: impl FnOnce(MainThreadMarker) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        // SAFETY: the main queue runs on the main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        work(mtm);
    });
}

/// Something Objective-C that is moved between threads by hand: the object is thread-safe for
/// what this module does with it (AVCaptureSession, VideoToolbox sessions), or it is only
/// touched on the main thread.
pub(super) struct Shared<T>(pub T);
// SAFETY: see the type's documentation; every use keeps to it.
unsafe impl<T> Send for Shared<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for Shared<T> {}

impl<T> Shared<T> {
    /// The value. A method, so a closure that calls it captures the whole wrapper (and so is
    /// `Send`), not the field.
    pub(super) fn get(&self) -> &T {
        &self.0
    }
}

/// The phone.
struct Ios;

impl Platform for Ios {
    fn request_permission(&self, permission: Permission) -> Result<(), String> {
        audio::request_permission(permission)
    }

    fn start_audio_session(&self) -> Result<(), String> {
        audio::start_session()
    }

    fn stop_audio_session(&self) {
        audio::stop_session();
    }

    fn set_speaker(&self, on: bool) -> Result<(), String> {
        audio::set_speaker(on)
    }

    fn start_camera(&self, request: CameraRequest, sink: FrameSink) -> Result<(), String> {
        audio::request_permission(Permission::Camera)?;
        camera::start(request, sink)?;
        audio::video_started();
        Ok(())
    }

    fn stop_camera(&self) {
        camera::stop();
    }

    fn start_screen_share(&self, sink: FrameSink) -> Result<(), String> {
        screen::start(sink)
    }

    fn stop_screen_share(&self) {
        screen::stop();
    }

    fn h264_encoder(&self, config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String> {
        Ok(Box::new(codec::Encoder::new(config)?))
    }

    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String> {
        Ok(Box::new(codec::Decoder::new()))
    }
}

impl WebSignIn for Ios {
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
        Ok(Box::new(web::open(page)?))
    }

    fn open_auth(&self, auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
        Ok(Box::new(auth::open(auth)?))
    }
}
