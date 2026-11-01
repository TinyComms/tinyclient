//! What a phone plugs into the shared core at startup, registered once from `android_main` and
//! `ios_main` before chatd starts:
//!
//! * mediad in process (`protocol::media_host`): chatd's calls, voice messages, sounds and the
//!   Settings device list run `mediad::inproc` on threads of the app instead of a binary.
//! * the call platform (`mediad::platform::Platform`): permissions, the audio session, camera,
//!   screen share and H.264.
//! * the web sign-in (`protocol::web_sign_in::WebSignIn`): Discord, Slack and Teams in an in-app
//!   web view, a Tiny server's OIDC page in the system's authentication session.
//!
//! The last two are stubs here that say plainly they are not built yet: the Android and iOS
//! work replaces `Phone`'s bodies (or registers platform types of its own in `register`) and
//! nothing else. Every rule a platform must keep (threads, blocking, frame formats) is in
//! `docs/mobile-platform.md` and on the traits themselves.

use mediad::platform::{CameraRequest, EncoderConfig, FrameSink, H264Decoder, H264Encoder, Permission, Platform};
use protocol::web_sign_in::{AuthHandle, AuthRequest, WebPage, WebPageHandle, WebSignIn};
use std::sync::Arc;

/// Said by every stub.
#[cfg_attr(target_os = "ios", allow(dead_code))]
const NOT_YET: &str = "This isn't available on this phone yet.";

/// The iOS phone (`mobile/ios`): AVFoundation, VideoToolbox, ReplayKit, WebKit and
/// AuthenticationServices behind the same two traits.
#[cfg(target_os = "ios")]
mod ios;

/// Registers everything. Idempotent enough to call once per launch.
pub fn register() {
    protocol::media_host::register(mediad::inproc::start);
    #[cfg(target_os = "ios")]
    {
        ios::register();
        return;
    }
    #[allow(unreachable_code)]
    let phone = Arc::new(Phone);
    mediad::platform::register(phone.clone());
    protocol::web_sign_in::register(phone);
}

/// The phone, until the platform code fills it in. iOS registers `ios` instead.
#[cfg_attr(target_os = "ios", allow(dead_code))]
struct Phone;

impl Platform for Phone {
    fn request_permission(&self, permission: Permission) -> Result<(), String> {
        log::warn!("tinyclient: {permission:?} permission is not wired to the platform yet");
        Err(NOT_YET.to_owned())
    }

    fn start_audio_session(&self) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }

    fn stop_audio_session(&self) {}

    fn set_speaker(&self, _on: bool) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }

    fn start_camera(&self, _request: CameraRequest, _sink: FrameSink) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }

    fn stop_camera(&self) {}

    fn start_screen_share(&self, _sink: FrameSink) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }

    fn stop_screen_share(&self) {}

    fn h264_encoder(&self, _config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String> {
        Err(NOT_YET.to_owned())
    }

    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String> {
        Err(NOT_YET.to_owned())
    }
}

impl WebSignIn for Phone {
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String> {
        log::warn!("tinyclient: the {} sign-in page is not wired to the platform yet", page.network);
        Err(NOT_YET.to_owned())
    }

    fn open_auth(&self, _auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String> {
        log::warn!("tinyclient: the authentication session is not wired to the platform yet");
        Err(NOT_YET.to_owned())
    }
}
