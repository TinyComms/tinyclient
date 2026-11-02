//! A phone's pictures, through the registered `platform`: its camera and screen deliver frames
//! to a sink that keeps the newest one, and its H.264 encoders and decoder code them. The capture
//! thread pulls a frame at 15 a second exactly as on a desktop, so `video` does not know which it
//! has. Built on every target so host tests drive it with `platform::fake`; only phones use it
//! as `picture::Outgoing` and `picture::Incoming`.
//!
//! A platform's encoder takes its bitrate when it is made, so a track's `Ask::bps` here only says
//! whether it runs: it stops or starts as on a desktop, and spends its full rate while it does.

use super::convert::{nv12, planes};
use super::{fit, rgb, Ask, Canvas, Encoded, Painted, Planes, CAMERA_HIGH, CAMERA_HIGH_BPS, CAMERA_LOW, CAMERA_LOW_BPS, CODEC_LATER, SHARE_BPS, SHARE_MAX};
use crate::platform::{self, CameraRequest, EncoderConfig, Facing, FrameSink, H264Decoder, H264Encoder, Permission, Platform, VideoFrame};
use std::sync::{Arc, Mutex};

const FPS: u32 = 15;

/// A capture running on the platform, and the newest frame it delivered.
struct Capture {
    platform: Arc<dyn Platform>,
    newest: Arc<Mutex<Option<VideoFrame>>>,
    camera: bool,
}

impl Capture {
    fn sink(newest: &Arc<Mutex<Option<VideoFrame>>>) -> FrameSink {
        let newest = newest.clone();
        Arc::new(move |frame: VideoFrame| {
            if frame.is_whole() {
                *newest.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(frame);
            }
        })
    }

    fn camera(platform: Arc<dyn Platform>) -> Result<Capture, String> {
        platform.request_permission(Permission::Camera)?;
        let newest = Arc::default();
        let request = CameraRequest { facing: Facing::Front, width: CAMERA_HIGH.0, height: CAMERA_HIGH.1, fps: FPS };
        platform.start_camera(request, Capture::sink(&newest))?;
        Ok(Capture { platform, newest, camera: true })
    }

    fn screen(platform: Arc<dyn Platform>) -> Result<Capture, String> {
        let newest = Arc::default();
        platform.start_screen_share(Capture::sink(&newest))?;
        Ok(Capture { platform, newest, camera: false })
    }

    /// The newest frame, or "later" while none has come yet (a camera warming up).
    fn frame(&self) -> Result<VideoFrame, String> {
        self.newest.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().ok_or_else(|| CODEC_LATER.to_owned())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.camera {
            self.platform.stop_camera();
        } else {
            self.platform.stop_screen_share();
        }
    }
}

/// One encoder at the size it was made for.
struct Coded {
    encoder: Box<dyn H264Encoder>,
    width: u32,
    height: u32,
}

/// The size and bitrate a layer is coded at, an encoder kept while the size stays.
fn coded<'a>(slot: &'a mut Option<Coded>, platform: &dyn Platform, width: u32, height: u32, bps: u32, screen: bool) -> Result<&'a mut Coded, String> {
    if !slot.as_ref().is_some_and(|coded| coded.width == width && coded.height == height) {
        let config = EncoderConfig { width, height, bitrate_bps: bps, fps: FPS, screen };
        *slot = Some(Coded { encoder: platform.h264_encoder(config)?, width, height });
    }
    Ok(slot.as_mut().expect("just made"))
}

/// Encodes one layer. An encoder that holds the frame for now is "later", which the call skips.
fn encode(coded: &mut Coded, planes: &Planes, key: bool) -> Result<Encoded, String> {
    let bytes = coded.encoder.encode(&nv12(planes), key)?;
    if bytes.is_empty() {
        return Err(CODEC_LATER.to_owned());
    }
    Ok(Encoded { bytes, key })
}

/// The encoders and the captures of what a call sends. Built on the first picture.
#[derive(Default)]
pub struct Outgoing {
    share: Option<Coded>,
    high: Option<Coded>,
    low: Option<Coded>,
    camera: Option<Capture>,
    screen: Option<Capture>,
}

impl Outgoing {
    /// One share frame, or why the screen could not be read.
    pub fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
        let platform = platform::required()?;
        if self.screen.is_none() {
            self.screen = Some(Capture::screen(platform.clone())?);
        }
        let grabbed = self.screen.as_ref().expect("just started").frame()?;
        let (width, height) = fit(grabbed.width, grabbed.height, SHARE_MAX.0, SHARE_MAX.1);
        let picture = planes(&grabbed, width, height);
        encode(coded(&mut self.share, &*platform, width, height, SHARE_BPS, true)?, &picture, ask.key)
    }

    /// The camera's two layers, high then low, or why the camera could not be read. A layer with
    /// no `Ask` is stopped: nothing is spent on it and it comes back empty.
    pub fn camera(&mut self, high: Option<Ask>, low: Option<Ask>) -> Result<(Encoded, Encoded), String> {
        let platform = platform::required()?;
        if self.camera.is_none() {
            self.camera = Some(Capture::camera(platform.clone())?);
        }
        let grabbed = self.camera.as_ref().expect("just started").frame()?;
        let (hw, hh) = fit(grabbed.width, grabbed.height, CAMERA_HIGH.0, CAMERA_HIGH.1);
        let (lw, lh) = fit(grabbed.width, grabbed.height, CAMERA_LOW.0, CAMERA_LOW.1);
        let high = match high {
            Some(ask) => encode(coded(&mut self.high, &*platform, hw, hh, CAMERA_HIGH_BPS, false)?, &planes(&grabbed, hw, hh), ask.key)?,
            None => Encoded::default(),
        };
        let low = match low {
            Some(ask) => encode(coded(&mut self.low, &*platform, lw, lh, CAMERA_LOW_BPS, false)?, &planes(&grabbed, lw, lh), ask.key)?,
            None => Encoded::default(),
        };
        Ok((high, low))
    }

    /// The button is off: a phone's camera light goes off with it.
    pub fn close_camera(&mut self) {
        self.camera = None;
        self.high = None;
        self.low = None;
    }

    /// The share is off: the platform's capture stops.
    pub fn close_share(&mut self) {
        self.screen = None;
        self.share = None;
    }
}

/// The one decoder a call holds, and the picture it last painted.
#[derive(Default)]
pub struct Incoming {
    decoder: Option<Box<dyn H264Decoder>>,
    canvas: Canvas,
}

impl Incoming {
    /// Drops a decoder whose sender changed, so the next frame starts from a keyframe.
    pub fn reset(&mut self) {
        self.decoder = None;
    }

    /// Decodes one received Annex B frame, and paints it if `paint`. Every frame goes through the
    /// decoder, painted or not: the next one is built on it. `Err` means it did not decode;
    /// `Ok(None)` that there is nothing to paint.
    #[allow(clippy::result_unit_err)]
    pub fn decode(&mut self, annex_b: &[u8], paint: bool) -> Result<Option<Painted>, ()> {
        if self.decoder.is_none() {
            self.decoder = platform::get().ok_or(())?.h264_decoder().ok();
        }
        let decoder = self.decoder.as_mut().ok_or(())?;
        let Some(decoded) = decoder.decode(annex_b).map_err(|_| ())? else {
            return Ok(None);
        };
        if !paint || decoded.width == 0 || decoded.height == 0 {
            return Ok(None);
        }
        let (width, height) = (decoded.width & !1, decoded.height & !1);
        let colors = rgb(&planes(&decoded, width, height));
        Ok(self.canvas.paint(&colors, width, height))
    }

    /// The file on disk goes.
    pub fn forget(&mut self) {
        self.canvas.forget();
    }
}

/// Whether the screen can be shared, before the button claims it: a platform that shares.
pub fn probe_screen() -> Result<(), String> {
    platform::required().map(|_| ())
}

/// Whether a camera can be used, before the button claims it: asks for the permission.
pub fn probe_camera() -> Result<(), String> {
    platform::required()?.request_permission(Permission::Camera)
}

/// Nothing is fetched on a phone: its codecs are the platform's.
pub fn warm() {}

/// Nothing is fetched on a phone.
pub fn retry_codec() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakePlatform;
    use std::time::{Duration, Instant};

    /// Pulls as the call loop does until a capture has delivered.
    fn until<T>(mut pull: impl FnMut() -> Result<T, String>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pull() {
                Ok(value) => return value,
                Err(words) if words == CODEC_LATER && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Err(words) => panic!("{words}"),
            }
        }
    }

    #[test]
    fn the_camera_and_the_screen_go_through_the_platform_and_come_back_as_a_picture() {
        let fake = Arc::new(FakePlatform::default());
        platform::register(fake.clone());
        let mut outgoing = Outgoing::default();
        let mut incoming = Incoming::default();
        let ask = Ask { key: true, ..Ask::default() };

        // Camera: asked for permission and the 640×360 front camera, coded in two layers.
        let (high, low) = until(|| outgoing.camera(Some(ask), Some(ask)));
        assert_eq!(*fake.log.permissions.lock().unwrap(), [Permission::Camera]);
        assert_eq!(fake.log.cameras.lock().unwrap()[0], CameraRequest { facing: Facing::Front, width: 640, height: 360, fps: 15 });
        let configs = fake.log.encoders.lock().unwrap().clone();
        assert_eq!(
            configs.iter().map(|c| (c.width, c.height, c.bitrate_bps, c.fps, c.screen)).collect::<Vec<_>>(),
            [(640, 360, CAMERA_HIGH_BPS, 15, false), (320, 180, CAMERA_LOW_BPS, 15, false)]
        );
        assert!(fake.camera_on());

        // What a peer sends back decodes, through the platform's decoder, into a JPEG.
        let painted = incoming.decode(&high.bytes, true).expect("the high layer decodes").expect("painted");
        assert_eq!((painted.width, painted.height), (640, 360));
        assert!(std::fs::metadata(&painted.path).unwrap().len() > 0);
        let smaller = incoming.decode(&low.bytes, true).expect("the low layer decodes").expect("painted");
        assert_eq!((smaller.width, smaller.height), (320, 180));
        incoming.forget();
        assert!(!smaller.path.exists());

        // A layer with no `Ask` is stopped.
        let (_, stopped) = until(|| outgoing.camera(Some(ask), None));
        assert!(stopped.bytes.is_empty());

        // The button off: the platform's camera stops.
        outgoing.close_camera();
        assert!(!fake.camera_on());

        // Share: the 1920×1080 screen fitted inside 1280×720, coded as screen content.
        let shared = until(|| outgoing.share(ask));
        assert!(fake.screen_on());
        let last = *fake.log.encoders.lock().unwrap().last().unwrap();
        assert_eq!((last.width, last.height, last.bitrate_bps, last.screen), (1280, 720, SHARE_BPS, true));
        assert!(!shared.bytes.is_empty());
        outgoing.close_share();
        assert!(!fake.screen_on());
        until(|| outgoing.share(ask));
        assert!(fake.screen_on(), "asked again, it starts again");
        drop(outgoing);
        assert!(!fake.screen_on(), "dropping the call's pictures stops the share");
    }
}
