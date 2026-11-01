//! The camera: an `AVCaptureSession` with the front or back wide-angle camera, NV12 frames to
//! mediad's sink, upright (portrait) and unmirrored.
//!
//! The simulator has no camera: `start` then says so in words the call screen shows, and the
//! call goes on without video.

use super::Shared;
use crate::gui::call::phone::hooks;
use dispatch2::{DispatchQueue, DispatchRetained};
use mediad::platform::{CameraRequest, Facing, FrameSink};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureDevicePosition,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureOutput, AVCaptureSession, AVCaptureSessionPreset640x480,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureVideoOrientation,
    AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use std::sync::Mutex;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no Drop needs beyond its
    // ivars.
    #[unsafe(super(NSObject))]
    #[name = "TinyCameraDelegate"]
    #[ivars = FrameSink]
    struct CameraDelegate;

    unsafe impl NSObjectProtocol for CameraDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CameraDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(&self, _output: &AVCaptureOutput, sample: &CMSampleBuffer, _connection: &AVCaptureConnection) {
            // SAFETY: the sample buffer is valid for the call.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            if let Some(frame) = super::pixels::frame_of(&image) {
                (self.ivars())(frame);
            }
        }
    }
);

impl CameraDelegate {
    fn new(sink: FrameSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(sink);
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }
}

/// A running capture.
struct Camera {
    session: Shared<Retained<AVCaptureSession>>,
    _delegate: Shared<Retained<CameraDelegate>>,
    _queue: Shared<DispatchRetained<DispatchQueue>>,
    request: CameraRequest,
    sink: FrameSink,
}

static CAMERA: Mutex<Option<Camera>> = Mutex::new(None);

/// The call screen's flip button.
pub(super) fn register_hooks() {
    hooks::route_flip(flip);
}

fn flip(front: bool) {
    let running = CAMERA.lock().ok().and_then(|held| held.as_ref().map(|camera| (camera.request, camera.sink.clone())));
    let Some((mut request, sink)) = running else {
        return;
    };
    request.facing = if front { Facing::Front } else { Facing::Back };
    // Off the UI thread: a capture session starts slowly.
    std::thread::spawn(move || {
        if let Err(words) = start(request, sink) {
            log::warn!("tinyclient: the camera did not flip: {words}");
        }
    });
}

/// Starts the camera and returns once it runs. A second start replaces the first.
pub(super) fn start(request: CameraRequest, sink: FrameSink) -> Result<(), String> {
    stop();
    let position = match request.facing {
        Facing::Front => AVCaptureDevicePosition::Front,
        Facing::Back => AVCaptureDevicePosition::Back,
    };
    // SAFETY: AVFoundation's documented capture setup; every object is used as its API says,
    // and the session is started and stopped off the main thread as Apple recommends.
    unsafe {
        let device = AVCaptureDevice::defaultDeviceWithDeviceType_mediaType_position(
            AVCaptureDeviceTypeBuiltInWideAngleCamera.expect("wide angle"),
            AVMediaTypeVideo,
            position,
        )
        .or_else(|| AVCaptureDevice::defaultDeviceWithMediaType(AVMediaTypeVideo.expect("video")))
        .ok_or_else(|| "This device has no camera.".to_owned())?;
        let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
            .map_err(|error| format!("The camera could not be opened ({}).", error.localizedDescription()))?;
        let session = AVCaptureSession::new();
        session.beginConfiguration();
        if session.canSetSessionPreset(AVCaptureSessionPreset640x480.expect("preset")) {
            session.setSessionPreset(AVCaptureSessionPreset640x480.expect("preset"));
        }
        if !session.canAddInput(&input) {
            return Err("The camera is in use by something else.".to_owned());
        }
        session.addInput(&input);

        let output = AVCaptureVideoDataOutput::new();
        let key: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
        let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[&*format as &AnyObject]);
        output.setVideoSettings(Some(&settings));
        output.setAlwaysDiscardsLateVideoFrames(true);
        let delegate = CameraDelegate::new(sink.clone());
        let queue = DispatchQueue::new("dev.tinyclient.camera", None);
        output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
        if !session.canAddOutput(&output) {
            return Err("The camera's pictures could not be read.".to_owned());
        }
        session.addOutput(&output);
        if let Some(connection) = output.connectionWithMediaType(AVMediaTypeVideo.expect("video")) {
            #[allow(deprecated)]
            if connection.isVideoOrientationSupported() {
                connection.setVideoOrientation(AVCaptureVideoOrientation::Portrait);
            }
        }
        session.commitConfiguration();
        session.startRunning();
        if !session.isRunning() {
            return Err("The camera did not start.".to_owned());
        }
        log::info!("tinyclient: camera running ({:?})", request.facing);
        *CAMERA.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Camera {
            session: Shared(session),
            _delegate: Shared(delegate),
            _queue: Shared(queue),
            request,
            sink,
        });
    }
    Ok(())
}

/// Stops the camera, if it runs.
pub(super) fn stop() {
    let camera = CAMERA.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    if let Some(camera) = camera {
        // SAFETY: stopping a session from a background thread, as Apple recommends.
        unsafe { camera.session.0.stopRunning() };
        log::info!("tinyclient: camera stopped");
    }
}
