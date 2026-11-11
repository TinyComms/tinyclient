//! The desktop's pictures: OpenH264 (Cisco's binary, fetched at run time), nokhwa's camera
//! (Media Foundation, AVFoundation, V4L2), and the screen through GDI, Core Graphics, the X11
//! root, or the Linux desktop portal. Moved here from `picture.rs`, which keeps what phones share.

use super::*;
use crate::rt;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, Level, Profile, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::{OpenH264API, Timestamp};
use openh264_sys2::{SBitrateInfo, ENCODER_OPTION_BITRATE, ENCODER_OPTION_FRAME_RATE, SPATIAL_LAYER_ALL};
use std::io::Read;
use std::path::Path;
use protocol::CameraSource;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};


impl YUVSource for Planes {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

struct Coded {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// What the encoder holds now.
    bps: u32,
    fps: f32,
    /// OpenH264 sets up on the first frame, and takes no options before that.
    begun: bool,
}

impl Coded {
    /// `bps` is also the most this encoder ever spends: [`Coded::tune`] only goes under it, and
    /// so is `fps`. A share's encoder is level 5.1, which holds 1440p at 60, and spreads its work
    /// over threads: one thread could not encode 1080p inside a frame at 60.
    fn open(width: u32, height: u32, bps: u32, fps: f32, screen: bool) -> Result<Coded, String> {
        let api = load_api()?;
        let threads = if screen { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8) as u16 } else { 1 };
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps))
            .rate_control_mode(RateControlMode::Bitrate)
            .max_frame_rate(FrameRate::from_hz(fps))
            .usage_type(if screen { UsageType::ScreenContentRealTime } else { UsageType::CameraVideoRealTime })
            .profile(Profile::Baseline)
            .level(if screen { Level::Level_5_1 } else { Level::Level_3_1 })
            // Above 30 a share trades some compression for the time: at medium complexity
            // eight threads took 14 ms on a 1080p frame, inside a frame at 30 but not at 60.
            .complexity(if screen && fps > 30. { Complexity::Low } else { Complexity::Medium })
            .num_threads(threads);
        let encoder = Encoder::with_api_config(api, config).map_err(|err| err.to_string())?;
        Ok(Coded { encoder, width, height, bps, fps, begun: false })
    }

    /// The rate and frame rate asked for, if they moved and the encoder can take them.
    fn tune(&mut self, ask: &Ask) {
        if !self.begun {
            return;
        }
        if ask.bps != self.bps && ask.bps > 0 {
            let mut info = SBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: ask.bps as i32 };
            // SAFETY: OpenH264 reads this option as an `SBitrateInfo`, which outlives the call,
            // and copies it. The encoder set up on its first frame.
            let refused = unsafe { self.encoder.raw_api().set_option(ENCODER_OPTION_BITRATE, (&raw mut info).cast()) };
            if refused == 0 {
                self.bps = ask.bps;
            } else {
                tracing::warn!("mediad: the encoder refused a bitrate of {}: {refused}", ask.bps);
            }
        }
        if ask.fps != self.fps && ask.fps > 0. {
            let mut fps = ask.fps;
            // SAFETY: as above, with a float, which is what this option is read as.
            let refused = unsafe { self.encoder.raw_api().set_option(ENCODER_OPTION_FRAME_RATE, (&raw mut fps).cast()) };
            if refused == 0 {
                self.fps = ask.fps;
            }
        }
    }

    fn encode(&mut self, planes: &Planes, ask: &Ask) -> Result<Encoded, String> {
        self.tune(ask);
        if ask.key && self.begun {
            self.encoder.force_intra_frame();
        }
        let stamp = Timestamp::from_millis(ask.at.as_millis() as u64);
        let encoded = self.encoder.encode_at(planes, stamp).map_err(|err| err.to_string())?;
        let key = matches!(encoded.frame_type(), FrameType::IDR | FrameType::I);
        let bytes = encoded.to_vec();
        self.begun = true;
        Ok(Encoded { bytes, key })
    }
}

/// The encoder for one track at this size: the one it has, or a new one, which starts with a
/// keyframe.
fn coded(slot: &mut Option<Coded>, width: u32, height: u32, nominal: u32, fps: f32, screen: bool) -> Result<&mut Coded, String> {
    if !slot.as_ref().is_some_and(|coded| coded.width == width && coded.height == height) {
        *slot = Some(Coded::open(width, height, nominal, fps, screen)?);
    }
    slot.as_mut().ok_or_else(|| "No encoder.".to_owned())
}

/// The encoders and the capture of what a call sends. Built on the first picture, not at join.
#[derive(Default)]
pub struct Outgoing {
    share: Option<Coded>,
    high: Option<Coded>,
    low: Option<Coded>,
    camera: Camera,
    cost: Cost,
    camera_cost: GrabCost,
    /// My own camera, painted for me.
    selfie: Selfie,
    /// My own share, painted for me. Apart from the camera's: both can be on at once.
    screen_selfie: Selfie,
}

/// One of my own pictures shrunk and written as a JPEG a few times a second, for my own tile.
#[derive(Default)]
struct Selfie {
    canvas: Canvas,
    at: Option<Instant>,
    generation: u64,
    painted: Option<Painted>,
}

impl Selfie {
    /// Paints this grab, unless the last was painted less than [`super::SELF_VIEW_EVERY`] ago.
    fn shoot(&mut self, grabbed: &Rgba) {
        if self.at.is_some_and(|at| at.elapsed() < super::SELF_VIEW_EVERY) {
            return;
        }
        self.at = Some(Instant::now());
        let (width, height) = fit(grabbed.width, grabbed.height, super::SELF_VIEW.0, super::SELF_VIEW.1);
        let rgb = super::screens::shrink(grabbed, width, height);
        self.generation = self.generation.wrapping_add(1);
        if let Some(painted) = self.canvas.paint(&rgb, width, height, self.generation) {
            self.painted = Some(painted);
        }
    }

    /// The last picture painted and not yet taken.
    fn take(&mut self) -> Option<Painted> {
        self.painted.take()
    }

    /// The picture's file goes, and the next shot is due at once.
    fn forget(&mut self) {
        self.canvas.forget();
        self.painted = None;
        self.at = None;
    }
}

/// What a camera frame costs to read, in the log once a second or so, with the rate the camera
/// gave them at: `camera.frame()` waits for the next one, so its time is the camera's period.
#[derive(Default)]
struct GrabCost {
    frames: u32,
    waited: Duration,
    since: Option<Instant>,
}

impl GrabCost {
    fn note(&mut self, waited: Duration, size: (u32, u32)) {
        self.frames += 1;
        self.waited += waited;
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        if now - since >= Duration::from_secs(1) {
            let seconds = (now - since).as_secs_f32();
            tracing::debug!(frames = self.frames, fps = self.frames as f32 / seconds, wait_ms = self.waited.as_secs_f32() * 1000. / self.frames as f32, ?size, "camera frames");
            *self = GrabCost { since: Some(now), ..GrabCost::default() };
        }
    }
}

/// What a share frame costs, in the log once a second or so: where the time goes when the rate
/// cannot be kept.
#[derive(Default)]
struct Cost {
    frames: u32,
    capture: Duration,
    convert: Duration,
    encode: Duration,
    since: Option<Instant>,
}

impl Cost {
    fn note(&mut self, began: Instant, captured: Instant, converted: Instant, encoded: Instant, grabbed: (u32, u32), sent: (u32, u32)) {
        self.frames += 1;
        self.capture += captured - began;
        self.convert += converted - captured;
        self.encode += encoded - converted;
        let since = *self.since.get_or_insert(began);
        if encoded - since >= Duration::from_secs(1) {
            let each = |total: Duration| total.as_secs_f32() * 1000. / self.frames as f32;
            tracing::debug!(
                frames = self.frames,
                capture_ms = each(self.capture),
                convert_ms = each(self.convert),
                encode_ms = each(self.encode),
                grabbed = ?grabbed,
                sent = ?sent,
                "share frame cost"
            );
            *self = Cost { since: Some(encoded), ..Cost::default() };
        }
    }
}

impl Outgoing {
    /// One share frame, or why the screen could not be read.
    pub fn share(&mut self, ask: Ask) -> Result<Encoded, String> {
        // A capture is wasted until the codec loads.
        if self.share.is_none() {
            load_api()?;
        }
        let quality = super::share_quality();
        let began = Instant::now();
        let grabbed = capture_screen()?;
        let captured = Instant::now();
        let (width, height) = fit(grabbed.width, grabbed.height, quality.max.0, quality.max.1);
        let planes = i420(&grabbed.pixels, grabbed.width, grabbed.height, width, height, grabbed.bgra);
        let converted = Instant::now();
        // The grab is the whole screen, so the preview costs one shrink a tenth of a second.
        self.screen_selfie.shoot(&grabbed);
        let encoded = coded(&mut self.share, width, height, quality.bps, quality.fps as f32, true)?.encode(&planes, &ask);
        self.cost.note(began, captured, converted, Instant::now(), (grabbed.width, grabbed.height), (width, height));
        encoded
    }

    /// My own camera as painted for me since last asked: a few times a second, the camera's
    /// grab shrunk and written as a JPEG.
    pub fn self_view(&mut self) -> Option<Painted> {
        self.selfie.take()
    }

    /// My own share as painted for me since last asked, on the same terms as [`Outgoing::self_view`].
    pub fn share_view(&mut self) -> Option<Painted> {
        self.screen_selfie.take()
    }

    /// The camera's two layers, high then low, or why the camera could not be read. A layer with
    /// no `Ask` is stopped: nothing is spent on it and it comes back empty.
    pub fn camera(&mut self, high: Option<Ask>, low: Option<Ask>) -> Result<(Encoded, Encoded), String> {
        if self.high.is_none() || self.low.is_none() {
            load_api()?;
        }
        let began = Instant::now();
        let grabbed = self.camera.frame()?;
        self.camera_cost.note(began.elapsed(), (grabbed.width, grabbed.height));
        self.selfie.shoot(&grabbed);
        let layer = |slot: &mut Option<Coded>, ask: Option<Ask>, most: (u32, u32), nominal: u32| match ask {
            Some(ask) => {
                let (width, height) = fit(grabbed.width, grabbed.height, most.0, most.1);
                let planes = i420(&grabbed.pixels, grabbed.width, grabbed.height, width, height, grabbed.bgra);
                coded(slot, width, height, nominal, FULL_FPS, false)?.encode(&planes, &ask)
            }
            None => Ok(Encoded::default()),
        };
        let high = layer(&mut self.high, high, CAMERA_HIGH, CAMERA_HIGH_BPS)?;
        let low = layer(&mut self.low, low, CAMERA_LOW, CAMERA_LOW_BPS)?;
        Ok((high, low))
    }

    pub fn close_camera(&mut self) {
        self.camera.close();
        self.selfie.forget();
    }

    /// The share is off. Drop the encoder, and on Linux close the portal session so the recording indicator does not stay up.
    pub fn close_share(&mut self) {
        self.share = None;
        self.screen_selfie.forget();
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        super::portal::stop_screen();
    }
}

/// The one decoder a call holds, and the painters that write what it decodes for the window.
#[derive(Default)]
pub struct Incoming {
    decoder: Option<Decoder>,
    painters: Painters,
}

impl Incoming {
    /// Drops a decoder whose sender changed, so the next frame starts from a keyframe.
    pub fn reset(&mut self) {
        self.decoder = None;
    }

    /// Decodes one received Annex B frame, and has it painted for `paint` if given. Every frame
    /// goes through the decoder, painted or not: the next one is built on it. `Err` means it did
    /// not decode. What was painted comes back from [`Incoming::painted`].
    #[allow(clippy::result_unit_err)]
    pub fn decode(&mut self, annex_b: &[u8], paint: Option<Tag>) -> Result<(), ()> {
        if self.decoder.is_none() {
            let api = load_api().map_err(|_| ())?;
            self.decoder = Decoder::with_api_config(api, DecoderConfig::new()).ok();
        }
        let decoder = self.decoder.as_mut().ok_or(())?;
        let Some(decoded) = decoder.decode(annex_b).map_err(|_| ())? else {
            return Ok(());
        };
        let Some(tag) = paint else {
            return Ok(());
        };
        let (width, height) = decoded.dimensions();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let mut colors = vec![0u8; width * height * 3];
        // This honours the decoder's strides, which are wider than the picture.
        decoded.write_rgb8(&mut colors);
        self.painters.paint(colors, width as u32, height as u32, tag);
        Ok(())
    }

    /// The pictures painted since this was last asked.
    pub fn painted(&mut self) -> Vec<(Tag, Painted)> {
        self.painters.painted()
    }

    /// The files on disk go, and so does anything still being painted.
    pub fn forget(&mut self) {
        self.painters.forget();
    }
}

/// The threads that write each decoded picture as a JPEG for the window, each with a file of
/// its own. One took 60 ms on a 1080p picture, two frames of a share at 30, so two or three of
/// them keep up with it; a picture that finds every painter busy is not painted, and the next
/// one is. Started at the first picture, not at join.
#[derive(Default)]
struct Painters {
    workers: Vec<Worker>,
    /// The painter to try first, so they take turns.
    next: usize,
    done: Option<(Sender<Done>, Receiver<Done>)>,
    /// Pictures from before the last `forget` are not shown.
    epoch: u64,
    generation: u64,
}

struct Worker {
    jobs: Sender<Job>,
    /// Painting now: a job sent to it would wait.
    busy: Arc<AtomicBool>,
}

enum Job {
    Paint { colors: Vec<u8>, width: u32, height: u32, generation: u64, epoch: u64, tag: Tag },
    /// The file goes.
    Forget,
}

struct Done {
    epoch: u64,
    tag: Tag,
    painted: Painted,
}

impl Painters {
    fn start(&mut self) {
        if !self.workers.is_empty() {
            return;
        }
        let (done_tx, done_rx) = channel();
        let count = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 3);
        for _ in 0..count {
            let (jobs, work) = channel::<Job>();
            let busy = Arc::new(AtomicBool::new(false));
            let (done, mine) = (done_tx.clone(), busy.clone());
            // A thread that could not start takes no jobs: a send to it fails, and the picture
            // is not painted.
            let _ = std::thread::Builder::new().name("mediad-paint".into()).spawn(move || {
                rt::worker_thread();
                let mut canvas = Canvas::default();
                for job in work {
                    match job {
                        Job::Paint { colors, width, height, generation, epoch, tag } => {
                            if let Some(painted) = canvas.paint(&colors, width, height, generation) {
                                let _ = done.send(Done { epoch, tag, painted });
                            }
                            mine.store(false, Ordering::Release);
                        }
                        Job::Forget => canvas.forget(),
                    }
                }
                canvas.forget();
            });
            self.workers.push(Worker { jobs, busy });
        }
        self.done = Some((done_tx, done_rx));
    }

    /// Hands the picture to a painter that is free, if any is.
    fn paint(&mut self, colors: Vec<u8>, width: u32, height: u32, tag: Tag) {
        self.start();
        let count = self.workers.len();
        for turn in 0..count {
            let at = (self.next + turn) % count;
            let worker = &self.workers[at];
            if worker.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
                continue;
            }
            self.next = (at + 1) % count;
            self.generation = self.generation.wrapping_add(1);
            let job = Job::Paint { colors, width, height, generation: self.generation, epoch: self.epoch, tag };
            if worker.jobs.send(job).is_err() {
                worker.busy.store(false, Ordering::Release);
            }
            return;
        }
    }

    fn painted(&mut self) -> Vec<(Tag, Painted)> {
        let epoch = self.epoch;
        self.done.as_ref().map_or_else(Vec::new, |(_, done)| done.try_iter().filter(|done| done.epoch == epoch).map(|done| (done.tag, done.painted)).collect())
    }

    fn forget(&mut self) {
        self.epoch += 1;
        for worker in &self.workers {
            let _ = worker.jobs.send(Job::Forget);
        }
        if let Some((_, done)) = &self.done {
            for _ in done.try_iter() {}
        }
    }
}

/// One try of the screen, so a button can say no before the call claims to be sharing.
pub fn probe_screen() -> Result<(), String> {
    capture_screen().map(|_| ())
}

/// The camera, through the system's own API (Media Foundation, AVFoundation, V4L2). On Linux, one failed open is retried through the desktop portal, which is how a PipeWire-held camera is reached on every distro. A cancel or a missing portal is remembered until the camera is closed, so a dark frame does not ask again.
#[derive(Default)]
struct Camera {
    inner: Option<nokhwa::Camera>,
    /// The choice it was opened with. A new choice closes it, and the next frame opens that.
    opened: Option<String>,
    /// The portal camera is the one producing frames.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    portal: bool,
    /// One portal attempt already failed or was cancelled.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    portal_gave_up: bool,
}

impl Camera {
    fn frame(&mut self) -> Result<Rgba, String> {
        let chosen = super::camera_chosen();
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        if self.portal {
            if self.opened != chosen {
                self.close();
            } else {
                return super::portal::capture_camera();
            }
        }
        if self.inner.is_some() && self.opened != chosen {
            self.close();
        }
        match self.open_and_read() {
            Ok(frame) => Ok(frame),
            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            Err(words) => self.portal_once(words, chosen),
            #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
            Err(words) => Err(words),
        }
    }

    fn open_and_read(&mut self) -> Result<Rgba, String> {
        self.open()?;
        let camera = self.inner.as_mut().ok_or_else(|| "No camera was opened.".to_owned())?;
        let frame = camera.frame().map_err(|err| err.to_string())?;
        rgba_of(&frame)
    }

    /// V4L2 could not open. Ask the portal once. A cancel is said in the portal's words. Anything else (no bus, no PipeWire, no camera there) keeps the V4L2 words.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn portal_once(&mut self, v4l2: String, chosen: Option<String>) -> Result<Rgba, String> {
        if self.portal_gave_up {
            return Err(v4l2);
        }
        match super::portal::open_camera() {
            Ok(()) => match super::portal::capture_camera() {
                Ok(frame) => {
                    if let Some(camera) = self.inner.as_mut() {
                        let _ = camera.stop_stream();
                    }
                    self.inner = None;
                    self.portal = true;
                    self.opened = chosen;
                    Ok(frame)
                }
                Err(words) => {
                    self.portal_gave_up = true;
                    super::portal::stop_camera();
                    Err(words)
                }
            },
            Err(err) if err.refused => {
                self.portal_gave_up = true;
                Err(err.words)
            }
            Err(_) => {
                self.portal_gave_up = true;
                Err(v4l2)
            }
        }
    }

    fn open(&mut self) -> Result<(), String> {
        if self.inner.is_some() {
            return Ok(());
        }
        let chosen = super::camera_chosen();
        let index = camera_index(chosen.as_deref())?;
        let mut camera = open_camera(index)?;
        camera.open_stream().map_err(|err| err.to_string())?;
        tracing::debug!(format = ?camera.camera_format(), "camera opened");
        self.inner = Some(camera);
        self.opened = chosen;
        Ok(())
    }

    fn close(&mut self) {
        if let Some(camera) = self.inner.as_mut() {
            let _ = camera.stop_stream();
        }
        self.inner = None;
        self.opened = None;
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            self.portal = false;
            self.portal_gave_up = false;
            super::portal::stop_camera();
        }
    }
}

/// Every camera the system lists, by id and name, with a small picture from each in `folder`
/// when one is given: Settings shows them. Each is opened for one frame.
pub fn cameras(folder: Option<&Path>) -> Vec<CameraSource> {
    {
        use nokhwa::utils::ApiBackend;
        let Ok(listed) = nokhwa::query(ApiBackend::Auto) else {
            return Vec::new();
        };
        let folder = folder.filter(|folder| std::fs::create_dir_all(folder).is_ok());
        if let Some(folder) = folder {
            for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "jpg") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|at| at.as_millis()).unwrap_or_default();
        listed
            .iter()
            .enumerate()
            .map(|(n, info)| {
                let mut source = CameraSource { id: camera_id(info), name: info.human_name(), thumbnail: String::new() };
                let Some(folder) = folder else {
                    return source;
                };
                let path = folder.join(format!("camera-{stamp}-{n}.jpg"));
                let picture = open_camera(info.index().clone())
                    .and_then(|mut camera| {
                        camera.open_stream().map_err(|err| err.to_string())?;
                        // The first frames of a camera that just woke are often dark.
                        let mut frame = camera.frame();
                        for _ in 0..4 {
                            frame = camera.frame();
                        }
                        let _ = camera.stop_stream();
                        frame.map_err(|err| err.to_string())
                    })
                    .and_then(|frame| rgba_of(&frame))
                    .and_then(|grabbed| super::screens::thumbnail(&grabbed, &path));
                match picture {
                    Ok(()) => source.thumbnail = path.to_string_lossy().into_owned(),
                    Err(err) => tracing::debug!("mediad: no picture of camera {}: {err}", source.name),
                }
                source
            })
            .collect()
    }
}

/// The camera `index`, at 720p at its fastest if it has 720p (more than the 640×360 that is
/// sent, and far less to decode than 4K), else at whatever it runs fastest at. The format is
/// nokhwa's to pick from those two asks: a format set by hand (`Exact`) on Windows came out as
/// black frames, and the frame rate Media Foundation reports back is a range minimum, not the
/// rate, so neither is trusted here; the rate is measured instead (`Outgoing::camera`).
fn open_camera(index: nokhwa::utils::CameraIndex) -> Result<nokhwa::Camera, String> {
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{RequestedFormat, RequestedFormatType, Resolution};
    let asks = [RequestedFormatType::HighestFrameRate(60), RequestedFormatType::HighestFrameRate(30), RequestedFormatType::HighestResolution(Resolution::new(1280, 720)), RequestedFormatType::AbsoluteHighestFrameRate];
    let mut last = String::new();
    for ask in asks {
        match nokhwa::Camera::new(index.clone(), RequestedFormat::new::<RgbFormat>(ask)) {
            Ok(mut camera) => {
                if let Ok(formats) = camera.compatible_camera_formats() {
                    tracing::debug!(count = formats.len(), formats = ?formats.iter().map(|f| format!("{}x{} {:?} {}fps", f.width(), f.height(), f.format(), f.frame_rate())).collect::<Vec<_>>(), "camera formats");
                }
                return Ok(camera);
            }
            Err(err) => last = err.to_string(),
        }
    }
    Err(last)
}

/// One camera frame as RGBA. An MJPEG frame is decoded here with the JPEG decoder this crate
/// already has: nokhwa's own needs a C build, which this crate does without, and without it
/// every MJPEG frame failed, which is what most webcams send at 720p and above.
fn rgba_of(frame: &nokhwa::Buffer) -> Result<Rgba, String> {
    use nokhwa::utils::FrameFormat;
    if frame.source_frame_format() == FrameFormat::MJPEG {
        let image = image::load_from_memory_with_format(frame.buffer(), image::ImageFormat::Jpeg).map_err(|err| err.to_string())?.into_rgba8();
        let (width, height) = (image.width(), image.height());
        return Ok(Rgba { width, height, pixels: image.into_raw(), bgra: false });
    }
    let image = frame.decode_image::<nokhwa::pixel_format::RgbFormat>().map_err(|err| err.to_string())?;
    let (width, height) = (image.width(), image.height());
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for pixel in image.pixels() {
        pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(Rgba { width, height, pixels, bgra: false })
}

/// The system's id of a listed camera: Media Foundation's symbolic link on Windows, else the
/// index. With the name, what `settings::CAMERA` stores.
fn camera_id(info: &nokhwa::utils::CameraInfo) -> String {
    let misc = info.misc();
    if misc.is_empty() {
        info.index().to_string()
    } else {
        misc
    }
}

/// Which camera `choice` means now: the one whose id matches, else whose name does, else the
/// first listed; with nothing listed, index 0, which says why it fails.
fn camera_index(choice: Option<&str>) -> Result<nokhwa::utils::CameraIndex, String> {
    use nokhwa::utils::{ApiBackend, CameraIndex};
    let listed = nokhwa::query(ApiBackend::Auto).map_err(|err| err.to_string())?;
    if listed.is_empty() {
        return Err("No camera was found.".to_owned());
    }
    let (name, id) = match choice.and_then(|choice| choice.split_once('\t')) {
        Some((name, id)) => (name, Some(id)),
        None => (choice.unwrap_or_default(), None),
    };
    let found = listed
        .iter()
        .find(|info| id.is_some_and(|id| camera_id(info) == id))
        .or_else(|| listed.iter().find(|info| !name.is_empty() && info.human_name() == name))
        .unwrap_or(&listed[0]);
    Ok(match found.index() {
        CameraIndex::Index(n) => CameraIndex::Index(*n),
        CameraIndex::String(s) => CameraIndex::String(s.clone()),
    })
}

/// The screen or window chosen for the share ([`super::choose_screen`]), else the primary
/// screen, as RGBA or BGRA. The whole of it, which the share then fits. On a Wayland session this
/// is what opens the desktop's picker.
pub fn capture_screen() -> Result<Rgba, String> {
    grab(super::screens::chosen().as_deref(), true)
}

/// One screen or window by its `--screens` id, for a thumbnail. This does not open a portal
/// dialog. Only Windows tells the ids apart; elsewhere a thumbnail is the primary screen, or the
/// frame a share that is already recording last produced.
pub(super) fn capture_source(id: Option<&str>) -> Result<Rgba, String> {
    grab(id, false)
}

fn grab(id: Option<&str>, allow_portal: bool) -> Result<Rgba, String> {
    #[cfg(windows)]
    {
        let _ = allow_portal;
        return super::screens::capture(id);
    }
    #[cfg(not(windows))]
    let _ = id;
    #[cfg(target_os = "macos")]
    {
        let _ = allow_portal;
        return capture_screen_macos();
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        return capture_screen_linux(allow_portal);
    }
    #[cfg(not(any(windows, target_os = "macos", all(target_os = "linux", not(target_env = "ohos")))))]
    {
        let _ = allow_portal;
        Err("Screen sharing isn't available on this system.".to_owned())
    }
}

#[cfg(target_os = "macos")]
fn capture_screen_macos() -> Result<Rgba, String> {
    use core_graphics::display::CGDisplay;
    let image = CGDisplay::main().image().ok_or_else(|| {
        "tinyclient can't record the screen. Allow it in System Settings › Privacy & Security › Screen & System Audio Recording.".to_owned()
    })?;
    let width = image.width() as u32;
    let height = image.height() as u32;
    let stride = image.bytes_per_row();
    let data = image.data();
    let bytes = data.bytes();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * stride;
        let row_bytes = bytes.get(start..start + width as usize * 4).ok_or_else(|| "The screen image was short.".to_owned())?;
        pixels.extend_from_slice(row_bytes);
    }
    Ok(Rgba { width, height, pixels, bgra: true })
}

/// Wayland goes to the portal first, on every compositor that implements it. A cancel stays a
/// cancel. The portal being absent may fall through to X11, which is XWayland on a Wayland
/// session and the real root on an X11 session. An X11 session never opens the portal, so XFCE
/// and the others do not get a dialog on top of a grab that already works. A thumbnail never
/// opens the portal either.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn capture_screen_linux(allow_portal: bool) -> Result<Rgba, String> {
    use super::session::Kind;
    if allow_portal && super::session::current() == Kind::Wayland {
        match super::portal::capture_screen() {
            Ok(frame) => return Ok(frame),
            Err(err) if err.refused => return Err(err.words),
            Err(err) => return x11_or(err.words),
        }
    }
    if !allow_portal {
        if let Some(frame) = super::portal::latest_screen() {
            return Ok(frame);
        }
        if super::session::current() == Kind::Wayland {
            return Err("The screen list does not open the share dialog.".to_owned());
        }
    }
    capture_x11()
}

/// The portal could not start. Use the X11 root when this session has a display.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn x11_or(portal_words: String) -> Result<Rgba, String> {
    let display = std::env::var("DISPLAY").ok();
    if display.is_some_and(|value| !value.is_empty()) {
        if let Ok(frame) = capture_x11() {
            return Ok(frame);
        }
    }
    Err(portal_words)
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn capture_x11() -> Result<Rgba, String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    let (conn, screen_num) = x11rb::connect(None).map_err(|_| "This session has no X11 display, so the screen can't be shared.".to_owned())?;
    let screen = &conn.setup().roots[screen_num];
    let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, screen.root, 0, 0, width, height, !0)
        .map_err(|err| err.to_string())?
        .reply()
        .map_err(|_| "The screen could not be read.".to_owned())?;
    if reply.depth < 24 {
        return Err("This screen's picture is in a form tinyclient can't share.".to_owned());
    }
    Ok(Rgba { width: width as u32, height: height as u32, pixels: reply.data, bgra: true })
}

const DOWNLOAD_FAILED: &str = "OpenH264 could not be downloaded. The call stays a voice call.";

enum Fetch {
    Idle,
    Busy,
    Failed(String),
}

fn fetch() -> &'static Mutex<Fetch> {
    static FETCH: OnceLock<Mutex<Fetch>> = OnceLock::new();
    FETCH.get_or_init(|| Mutex::new(Fetch::Idle))
}

/// Starts the download when the binary is not here yet, and returns at once.
pub fn warm() {
    let Ok(path) = library_path() else {
        return;
    };
    if path.exists() {
        return;
    }
    start_download(path);
}

/// A failed download may be tried again, the next time share or camera is turned on.
pub fn retry_codec() {
    let Ok(mut state) = fetch().lock() else {
        return;
    };
    if matches!(*state, Fetch::Failed(_)) {
        *state = Fetch::Idle;
    }
}

fn start_download(path: PathBuf) {
    let Ok(mut state) = fetch().lock() else {
        return;
    };
    if !matches!(*state, Fetch::Idle) {
        return;
    }
    *state = Fetch::Busy;
    drop(state);
    let started = std::thread::Builder::new().name("openh264".into()).spawn(move || {
        let result = download(&path);
        if let Ok(mut state) = fetch().lock() {
            *state = match result {
                Ok(()) => Fetch::Idle,
                Err(err) => Fetch::Failed(err),
            };
        }
    });
    if started.is_err() {
        if let Ok(mut state) = fetch().lock() {
            *state = Fetch::Failed(DOWNLOAD_FAILED.to_owned());
        }
    }
}

/// Cisco's OpenH264 2.6.0 for this machine. A download in progress is [`CODEC_LATER`], not a
/// failure, and this never blocks on the network.
fn load_api() -> Result<OpenH264API, String> {
    let path = library_path()?;
    if let Some(api) = open_cached(&path) {
        return Ok(api);
    }
    match fetch().lock() {
        Ok(state) => match &*state {
            Fetch::Busy => return Err(CODEC_LATER.to_owned()),
            Fetch::Failed(err) => return Err(err.clone()),
            Fetch::Idle => {}
        },
        Err(_) => return Err(DOWNLOAD_FAILED.to_owned()),
    }
    start_download(path);
    Err(CODEC_LATER.to_owned())
}

fn open_cached(path: &Path) -> Option<OpenH264API> {
    if !path.exists() {
        return None;
    }
    match OpenH264API::from_blob_path(path) {
        Ok(api) => Some(api),
        Err(_) => {
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn library_path() -> Result<PathBuf, String> {
    let name = library_name().ok_or_else(|| "OpenH264 has no build for this system.".to_owned())?;
    let dir = if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("tinyclient")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("tinyclient")
    } else {
        std::env::temp_dir().join("tinyclient")
    };
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join(name))
}

fn library_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("openh264-2.6.0-win64.dll")
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        Some("openh264-2.6.0-win-arm64.dll")
    } else if cfg!(all(windows, target_arch = "x86")) {
        Some("openh264-2.6.0-win32.dll")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("libopenh264-2.6.0-linux64.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("libopenh264-2.6.0-linux-arm64.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        Some("libopenh264-2.6.0-linux-arm.8.so")
    } else if cfg!(all(target_os = "linux", target_arch = "x86")) {
        Some("libopenh264-2.6.0-linux32.8.so")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("libopenh264-2.6.0-mac-arm64.dylib")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("libopenh264-2.6.0-mac-x64.dylib")
    } else {
        None
    }
}

fn library_url(name: &str) -> String {
    format!("https://ciscobinary.openh264.org/{name}.bz2")
}

fn download(path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| "OpenH264 has no name.".to_owned())?;
    let url = library_url(name);
    let response = ureq::get(&url).timeout(Duration::from_secs(20)).call().map_err(|err| {
        tracing::warn!("mediad: OpenH264 download failed: {err}");
        DOWNLOAD_FAILED.to_owned()
    })?;
    let mut packed = Vec::new();
    response.into_reader().read_to_end(&mut packed).map_err(|err| err.to_string())?;
    let mut plain = Vec::new();
    bzip2_rs::DecoderReader::new(&packed[..]).read_to_end(&mut plain).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &plain).map_err(|err| err.to_string())?;
    std::fs::rename(&tmp, path).map_err(|err| err.to_string())?;
    Ok(())
}

/// Whether a camera can be opened, said before the button claims it is on.
pub fn probe_camera() -> Result<(), String> {
    let mut camera = Camera::default();
    camera.frame().map(|_| ())?;
    camera.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rgba {
        Rgba { width: 1920, height: 1080, pixels: vec![200; 1920 * 1080 * 4], bgra: true }
    }

    #[test]
    fn a_self_view_is_shrunk_to_the_corner_size_painted_at_a_steady_rate_and_let_go() {
        let mut outgoing = Outgoing::default();
        outgoing.screen_selfie.shoot(&screen());
        let first = outgoing.share_view().expect("the first grab is painted at once");
        assert_eq!((first.width, first.height), super::super::SELF_VIEW);
        assert!(first.path.exists());
        assert!(outgoing.share_view().is_none(), "taken once");
        assert!(outgoing.self_view().is_none(), "the camera's slot is a different one");

        outgoing.screen_selfie.shoot(&screen());
        assert!(outgoing.share_view().is_none(), "a grab inside the interval is not painted");

        outgoing.screen_selfie.at = None;
        outgoing.screen_selfie.shoot(&screen());
        let next = outgoing.share_view().unwrap();
        assert_ne!(next.generation, first.generation);
        assert!(!first.path.exists(), "the file before it is gone");

        outgoing.screen_selfie.forget();
        assert!(!next.path.exists(), "and so is the last one when the share stops");
    }

    #[test]
    fn openh264_is_fetched_from_cisco() {
        let name = library_name().unwrap_or("openh264-2.6.0-win64.dll");
        let url = library_url(name);
        assert!(url.starts_with("https://ciscobinary.openh264.org/"), "{url}");
        assert!(url.ends_with(".bz2"), "{url}");
        assert!(url.contains(name), "{url}");
        assert!(!url.contains("github.com"), "{url}");
    }

    /// A camera, or a screen, through real OpenH264: `cargo test -p mediad --lib -- --ignored`
    /// on a machine that has the binary.
    #[test]
    #[ignore = "needs the OpenH264 binary, which is not in the tree"]
    fn frames_survive_the_real_codec_in_bitrate_mode_and_the_rate_moves() {
        let (width, height) = (640usize, 360usize);
        let mut outgoing = Coded::open(width as u32, height as u32, CAMERA_HIGH_BPS, FULL_FPS, false).unwrap();
        let mut incoming = Incoming::default();
        let (mut key, mut bytes_high, mut bytes_low) = (false, 0usize, 0usize);
        for n in 0..90usize {
            // Noise that moves, so the encoder has to work for its bits.
            let mut rgba = vec![0u8; width * height * 4];
            for (at, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let value = ((at * 2654435761 + n * 40503) >> 7) as u8;
                pixel.copy_from_slice(&[value, value / 2, 255 - value, 255]);
            }
            let planes = i420(&rgba, width as u32, height as u32, width as u32, height as u32, false);
            let bps = if n < 45 { CAMERA_HIGH_BPS } else { CAMERA_LOW_BPS };
            let ask = Ask { key: n == 30, bps, at: Duration::from_millis(n as u64 * 66), fps: FULL_FPS };
            let encoded = outgoing.encode(&planes, &ask).unwrap();
            if n == 0 || n == 30 {
                assert!(encoded.key, "frame {n} was asked to be a keyframe");
                key = true;
            }
            if n > 0 && n != 30 {
                assert!(!encoded.key, "uiIntraPeriod 0 makes no keyframe of its own at {n}");
            }
            if (5..45).contains(&n) && n != 30 {
                bytes_high += encoded.bytes.len();
            }
            if n >= 55 {
                bytes_low += encoded.bytes.len();
            }
            if !encoded.bytes.is_empty() {
                let tag = (n == 0).then(|| Tag { user: "ada".into(), kind: protocol::PictureKind::Share });
                incoming.decode(&encoded.bytes, tag).expect("every frame decodes");
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut painted = incoming.painted();
                while n == 0 && painted.is_empty() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                    painted = incoming.painted();
                }
                assert_eq!(!painted.is_empty(), n == 0, "only what was asked is painted, at frame {n}");
            }
        }
        incoming.forget();
        assert!(key);
        assert!(bytes_low * 45 / 35 < bytes_high, "the rate moved: {bytes_high} then {bytes_low}");
    }
}
