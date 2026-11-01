//! What a phone does for mediad that a desktop does itself: permissions, the audio session,
//! the camera, the screen, and H.264.
//!
//! A desktop mediad opens the camera (nokhwa), grabs the screen (GDI, CoreGraphics, X11) and
//! codes H.264 with Cisco's OpenH264, downloaded at run time. None of that exists or is allowed on
//! a phone, so the mobile entry registers a [`Platform`] at startup ([`register`]) and mediad asks
//! it instead (`picture`, `permission`, the call loops). Sound itself stays cpal (AAudio on
//! Android, CoreAudio on iOS).
//!
//! Threading. mediad calls every method from its own threads (the call's loop, a request
//! thread), never a platform UI thread, and may block on it: the platform hops to its main
//! thread itself when it must. `request_permission` blocks until the user answers. A
//! [`FrameSink`] may be called from any thread the platform likes (the capture queue), as often
//! as frames come; it only stores the frame and returns at once. Encoders and decoders are used
//! from one thread at a time (the call's loop) and must answer each call within a frame time
//! (about 60 ms); they are `Send`, not `Sync`.
//!
//! See `docs/mobile-platform.md`.

use std::sync::{Arc, RwLock};

pub mod fake;

/// Which permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Microphone,
    Camera,
}

/// Which camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Facing {
    #[default]
    Front,
    Back,
}

/// How a [`VideoFrame`]'s bytes are laid out. Always tightly packed (stride = width), rows top
/// down; a platform with padded rows copies them out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Y plane, then the U plane, then the V plane, each chroma plane a quarter of Y.
    I420,
    /// Y plane, then one plane of interleaved U and V (CbCr) bytes. What both phone cameras
    /// produce natively and both hardware encoders take: Android
    /// `COLOR_FormatYUV420SemiPlanar`, iOS `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
    Nv12,
    /// Four bytes a pixel: R, G, B, A.
    Rgba,
    /// Four bytes a pixel: B, G, R, A. A screen capture usually.
    Bgra,
}

/// One picture. Width and height are even for I420 and NV12.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    /// When it was captured, in microseconds on any steady clock. Informational.
    pub timestamp_us: u64,
}

impl VideoFrame {
    /// The bytes a frame of this size and format must have.
    pub fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
        let pixels = width as usize * height as usize;
        match format {
            PixelFormat::I420 | PixelFormat::Nv12 => pixels + 2 * ((width as usize / 2) * (height as usize / 2)),
            PixelFormat::Rgba | PixelFormat::Bgra => pixels * 4,
        }
    }

    /// True when `data` is as long as the size and format say.
    pub fn is_whole(&self) -> bool {
        self.data.len() == Self::expected_len(self.width, self.height, self.format)
    }
}

/// Where a camera or a screen share delivers its frames. Store and return; never block.
pub type FrameSink = Arc<dyn Fn(VideoFrame) + Send + Sync>;

/// The camera a call asks for. The platform picks the nearest size it has; mediad scales.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraRequest {
    pub facing: Facing,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// One H.264 encoder: constrained baseline profile, level 3.1, Annex B out, one slice a frame
/// preferred, no B-frames, SPS and PPS before every keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    /// Target bitrate, bits a second.
    pub bitrate_bps: u32,
    pub fps: u32,
    /// Screen content (sharper text, fewer frames) rather than a camera.
    pub screen: bool,
}

/// A hardware or software H.264 encoder (MediaCodec, VideoToolbox).
pub trait H264Encoder: Send {
    /// Encodes one frame of the configured size, in NV12 (the frame mediad gives is always
    /// NV12 at exactly `EncoderConfig::width` × `height`). Returns Annex B (start codes, NAL
    /// units), or an empty vector when the encoder holds the frame and has nothing out yet.
    /// `keyframe` asks for an IDR now.
    fn encode(&mut self, frame: &VideoFrame, keyframe: bool) -> Result<Vec<u8>, String>;
}

/// A hardware or software H.264 decoder.
pub trait H264Decoder: Send {
    /// Decodes one Annex B access unit. `Ok(None)` when it needs more (it waits for a keyframe);
    /// `Ok(Some)` a picture in I420 or NV12 (or RGBA/BGRA), tightly packed.
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String>;
}

/// Everything a phone does for mediad. Words in `Err` are shown to the user as they are.
pub trait Platform: Send + Sync {
    /// Asks for `permission`, blocking until the user answers or it is known. `Ok` when granted.
    fn request_permission(&self, permission: Permission) -> Result<(), String>;

    /// A call is starting: iOS `AVAudioSession` `.playAndRecord` with mode `.voiceChat`, active;
    /// Android `AudioManager.MODE_IN_COMMUNICATION` with audio focus. Before cpal opens devices.
    fn start_audio_session(&self) -> Result<(), String>;
    /// The call ended: deactivate the session, give focus back, `MODE_NORMAL`.
    fn stop_audio_session(&self);
    /// The loudspeaker (`true`) or the earpiece (`false`) while a call's session is active.
    fn set_speaker(&self, on: bool) -> Result<(), String>;

    /// Starts the camera and returns once it delivers or fails. Frames go to `sink` until
    /// [`Platform::stop_camera`]. A second start replaces the first.
    fn start_camera(&self, request: CameraRequest, sink: FrameSink) -> Result<(), String>;
    fn stop_camera(&self);

    /// Starts sharing the screen (iOS ReplayKit, Android MediaProjection), frames to `sink`.
    fn start_screen_share(&self, sink: FrameSink) -> Result<(), String>;
    fn stop_screen_share(&self);

    fn h264_encoder(&self, config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String>;
    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String>;
}

static PLATFORM: RwLock<Option<Arc<dyn Platform>>> = RwLock::new(None);

/// Registers the phone's platform. A later registration replaces an earlier one.
pub fn register(platform: Arc<dyn Platform>) {
    *PLATFORM.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(platform);
}

/// The registered platform.
pub fn get() -> Option<Arc<dyn Platform>> {
    PLATFORM.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Said when a phone build has no platform registered.
pub const MISSING: &str = "This phone build has no camera, screen or video codec support yet.";

/// The registered platform, or [`MISSING`] in words.
pub fn required() -> Result<Arc<dyn Platform>, String> {
    get().ok_or_else(|| MISSING.to_owned())
}

/// Holds a call's audio session while it lives: started by the call's loop, stopped when the
/// loop ends however it ends. Nothing on a desktop, or when no platform is registered.
pub struct AudioSession(Option<Arc<dyn Platform>>);

/// Starts the call's audio session on a phone. A session that fails to start is logged and the
/// call goes on: cpal may still open the devices.
pub fn call_audio() -> AudioSession {
    if !cfg!(any(target_os = "android", target_os = "ios")) {
        return AudioSession(None);
    }
    let Some(platform) = get() else {
        return AudioSession(None);
    };
    if let Err(words) = platform.start_audio_session() {
        tracing::warn!("mediad: the audio session did not start: {words}");
    }
    AudioSession(Some(platform))
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        if let Some(platform) = self.0.take() {
            platform.stop_audio_session();
        }
    }
}
