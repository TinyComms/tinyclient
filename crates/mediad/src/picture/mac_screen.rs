use super::{fit, Rgba};
use block2::RcBlock;
use core_graphics::display::CGDisplay;
use dispatch2::DispatchQueue;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSError, NSObject};
use objc2_screen_capture_kit::{
    SCContentFilter, SCRunningApplication, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use protocol::ScreenSource;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const PERMISSION: &str = "Allow tinyclient in System Settings › Privacy & Security › Screen & System Audio Recording, then restart tinyclient.";
const GONE: &str = "The shared window closed or is no longer available.";
const NO_DISPLAY: &str = "The selected display is no longer available.";
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const WAIT: Duration = Duration::from_secs(4);
const WINDOW_CHECK: Duration = Duration::from_millis(750);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

fn available() -> bool {
    AnyClass::get(c"SCStream").is_some()
}

pub(super) fn listed() -> Vec<ScreenSource> {
    if !available() || !unsafe { CGPreflightScreenCaptureAccess() } {
        // Keep Share available so starting one can open the system's permission prompt.
        return super::screens::default_source();
    }
    autoreleasepool(|_| {
        let Ok(content) = content(true) else {
            return Vec::new();
        };
        let main = CGDisplay::main().id;
        let mut sources = Vec::new();
        // The content snapshot is immutable. Its objects remain retained throughout enumeration.
        unsafe {
            for (index, display) in content.displays().iter().enumerate() {
                let primary = display.displayID() == main;
                let label = if primary { " (main)" } else { "" };
                sources.push(ScreenSource {
                    id: format!("display:{}", display.displayID()),
                    name: format!("Screen {}{label}", index + 1),
                    primary,
                    ..ScreenSource::default()
                });
            }
            for window in content.windows().iter().filter(|window| !ours(window)) {
                if let Some(title) = window
                    .title()
                    .map(|title| title.to_string())
                    .filter(|title| !title.trim().is_empty())
                {
                    sources.push(ScreenSource {
                        id: format!("window:{}", window.windowID()),
                        name: title,
                        window: true,
                        ..ScreenSource::default()
                    });
                }
            }
        }
        sources
    })
}

fn ours(window: &SCWindow) -> bool {
    unsafe { window.owningApplication().as_deref().is_some_and(our_app) }
}

fn our_app(app: &SCRunningApplication) -> bool {
    let picker_pid = std::env::var("TINYCLIENT_SHARE_PICKER_PID")
        .ok()
        .and_then(|pid| pid.parse::<i32>().ok());
    // The picker and call pop-out belong to the same application as the main window.
    unsafe {
        picker_pid == Some(app.processID())
            || app.bundleIdentifier().to_string() == "dev.tinyclient.app"
            || app
                .applicationName()
                .to_string()
                .eq_ignore_ascii_case("tinyclient")
    }
}

fn content(on_screen_only: bool) -> Result<Retained<SCShareableContent>, String> {
    let (send, receive) = mpsc::sync_channel(1);
    let done = RcBlock::new(
        move |found: *mut SCShareableContent, error: *mut NSError| unsafe {
            // Retain the immutable snapshot before returning it from ScreenCaptureKit's callback.
            let result = Retained::retain(found).ok_or_else(|| {
                error.as_ref().map_or_else(
                    || PERMISSION.to_owned(),
                    |error| error.localizedDescription().to_string(),
                )
            });
            let _ = send.try_send(result);
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, on_screen_only, &done,
        );
    }
    receive
        .recv_timeout(WAIT)
        .map_err(|_| "ScreenCaptureKit did not return the available sources.".to_owned())?
}

#[derive(Default)]
struct FrameState {
    frame: Option<Rgba>,
    error: Option<String>,
}

#[derive(Default)]
struct Frames {
    state: Mutex<FrameState>,
    ready: Condvar,
}

impl Frames {
    fn frame(&self) -> Result<Rgba, String> {
        let state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        let (state, _) = self
            .ready
            .wait_timeout_while(state, WAIT, |state| {
                state.frame.is_none() && state.error.is_none()
            })
            .unwrap_or_else(|err| err.into_inner());
        match &state.error {
            Some(error) => Err(error.clone()),
            None => state
                .frame
                .clone()
                .ok_or_else(|| "The selected source did not draw a picture.".to_owned()),
        }
    }

    fn fail(&self, error: String) {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .error = Some(error);
        self.ready.notify_all();
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TinyMediadScreenOutput"]
    #[ivars = Arc<Frames>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(&self, _: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Screen {
                if let Some(frame) = copy_pixels(sample) {
                    let frames = self.ivars();
                    frames.state.lock().unwrap_or_else(|err| err.into_inner()).frame = Some(frame);
                    frames.ready.notify_all();
                }
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_didStopWithError(&self, _: &SCStream, error: &NSError) {
            self.ivars().fail(error.localizedDescription().to_string());
        }

        #[allow(non_snake_case)]
        #[unsafe(method(streamDidBecomeInactive:))]
        fn streamDidBecomeInactive(&self, _: &SCStream) {
            self.ivars().fail(GONE.to_owned());
        }
    }
);

fn copy_pixels(sample: &CMSampleBuffer) -> Option<Rgba> {
    // The callback owns a retained sample until it returns. Lock the packed BGRA buffer while
    // copying the rows, skipping Core Video's stride padding, then release it back to the stream.
    unsafe {
        let buffer = sample.image_buffer()?;
        if CVPixelBufferGetPixelFormatType(&buffer) != BGRA
            || CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) != 0
        {
            return None;
        }
        let width = CVPixelBufferGetWidth(&buffer);
        let height = CVPixelBufferGetHeight(&buffer);
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let frame = if base.is_null() || width == 0 || height == 0 || stride < width * 4 {
            None
        } else {
            let mut pixels = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                pixels.extend_from_slice(std::slice::from_raw_parts(
                    base.add(row * stride),
                    width * 4,
                ));
            }
            Some(Rgba {
                width: width as u32,
                height: height as u32,
                pixels,
                bgra: true,
            })
        };
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
        frame
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Window(u32),
    Display(u32),
}

impl Source {
    fn parse(id: Option<&str>) -> Result<Self, String> {
        match id {
            None | Some("display:primary") => Ok(Self::Display(CGDisplay::main().id)),
            Some(id) => {
                if let Some(raw) = id.strip_prefix("window:") {
                    return raw.parse().map(Self::Window).map_err(|_| GONE.to_owned());
                }
                id.strip_prefix("display:")
                    .and_then(|raw| raw.parse().ok())
                    .map(Self::Display)
                    .ok_or_else(|| NO_DISPLAY.to_owned())
            }
        }
    }

    fn target(
        self,
        content: &SCShareableContent,
    ) -> Result<(Retained<SCContentFilter>, (u32, u32)), String> {
        // Pass only objects from the same content snapshot to ScreenCaptureKit.
        unsafe {
            match self {
                Self::Window(id) => {
                    let window = content
                        .windows()
                        .iter()
                        .find(|window| window.windowID() == id && !ours(window))
                        .ok_or_else(|| GONE.to_owned())?;
                    let filter = SCContentFilter::initWithDesktopIndependentWindow(
                        SCContentFilter::alloc(),
                        &window,
                    );
                    Ok((filter, window_size(&window)))
                }
                Self::Display(id) => {
                    let display = content
                        .displays()
                        .iter()
                        .find(|display| display.displayID() == id)
                        .ok_or_else(|| NO_DISPLAY.to_owned())?;
                    let apps = content
                        .applications()
                        .iter()
                        .filter(|app| our_app(app))
                        .collect::<Vec<_>>();
                    let excluded = NSArray::from_retained_slice(&apps);
                    let filter =
                        SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                            SCContentFilter::alloc(),
                            &display,
                            &excluded,
                            &NSArray::new(),
                        );
                    let mode = CGDisplay::new(id).display_mode();
                    let size = mode
                        .map_or((display.width() as u32, display.height() as u32), |mode| {
                            (mode.pixel_width() as u32, mode.pixel_height() as u32)
                        });
                    Ok((filter, size))
                }
            }
        }
    }
}

fn window_size(window: &SCWindow) -> (u32, u32) {
    let size = unsafe { window.frame() }.size;
    (size.width.ceil() as u32, size.height.ceil() as u32)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Options {
    source: Source,
    max: (u32, u32),
    fps: u32,
    cursor: bool,
}

impl Options {
    fn configuration(self, (width, height): (u32, u32)) -> Retained<SCStreamConfiguration> {
        let (width, height) = fit(width, height, self.max.0, self.max.1);
        unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(width as usize);
            config.setHeight(height as usize);
            config.setPixelFormat(BGRA);
            config.setMinimumFrameInterval(CMTime::new(1, self.fps as i32));
            config.setQueueDepth(3);
            config.setShowsCursor(self.cursor);
            config
        }
    }
}

fn stream_result(
    start: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), String> {
    let (send, receive) = mpsc::sync_channel(1);
    let done = RcBlock::new(move |error: *mut NSError| {
        let result = unsafe { error.as_ref() }.map_or(Ok(()), |error| {
            Err(error.localizedDescription().to_string())
        });
        let _ = send.try_send(result);
    });
    start(&done);
    receive
        .recv_timeout(WAIT)
        .map_err(|_| "ScreenCaptureKit did not respond.".to_owned())?
}

fn record(options: Options, frames: Arc<Frames>, stop: Receiver<()>) -> Result<(), String> {
    let content = content(false)?;
    let (filter, mut size) = options.source.target(&content)?;
    let output = Output::alloc().set_ivars(frames);
    let output: Retained<Output> = unsafe { msg_send![super(output), init] };
    let queue = DispatchQueue::new("dev.tinyclient.screen.frames", None);
    let stream = unsafe {
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &options.configuration(size),
            Some(ProtocolObject::from_ref(&*output)),
        );
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|error| error.localizedDescription().to_string())?;
        stream
    };
    if let Err(error) =
        stream_result(|done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) })
    {
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
        return Err(error);
    }
    let result = loop {
        match stop.recv_timeout(WINDOW_CHECK) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Source::Window(id) = options.source {
            let updated = autoreleasepool(|_| {
                let content = self::content(false)?;
                let window = unsafe {
                    content
                        .windows()
                        .iter()
                        .find(|window| window.windowID() == id)
                }
                .ok_or_else(|| GONE.to_owned())?;
                Ok(window_size(&window))
            });
            match updated {
                Ok(next) if next != size && next.0 > 0 && next.1 > 0 => {
                    if let Err(error) = stream_result(|done| unsafe {
                        stream.updateConfiguration_completionHandler(
                            &options.configuration(next),
                            Some(done),
                        );
                    }) {
                        break Err(error);
                    }
                    size = next;
                }
                Err(error) => break Err(error),
                _ => {}
            }
        }
    };
    unsafe { stream.stopCaptureWithCompletionHandler(None) };
    result
}

struct Live {
    options: Options,
    frames: Arc<Frames>,
    // Disconnecting wakes the stream's owning thread, which stops the capture.
    _stop: Sender<()>,
}

impl Live {
    fn open(options: Options) -> Result<Self, String> {
        let frames = Arc::new(Frames::default());
        let output = frames.clone();
        let (stop, stopped) = mpsc::channel();
        std::thread::Builder::new()
            .name("screen-capture-kit".into())
            .spawn(move || {
                if let Err(error) = autoreleasepool(|_| record(options, output.clone(), stopped)) {
                    output.fail(error);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            options,
            frames,
            _stop: stop,
        })
    }
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

pub(super) fn stop() {
    LIVE.lock().unwrap_or_else(|err| err.into_inner()).take();
}

pub(super) fn capture(id: Option<&str>, live: bool) -> Result<Rgba, String> {
    let source = Source::parse(id)?;
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        if live {
            unsafe { CGRequestScreenCaptureAccess() };
        }
        return Err(PERMISSION.to_owned());
    }
    if !available() {
        let Source::Display(display) = source else {
            return Err(GONE.to_owned());
        };
        return legacy_display(display);
    }
    if !live {
        let thumb = Options {
            source,
            max: super::screens::THUMB,
            fps: 30,
            cursor: false,
        };
        return Live::open(thumb)?.frames.frame();
    }
    let quality = super::share_quality();
    let options = Options {
        source,
        max: quality.max,
        fps: quality.fps,
        cursor: super::cursor_shown(),
    };
    let mut active = LIVE.lock().unwrap_or_else(|err| err.into_inner());
    if active
        .as_ref()
        .is_none_or(|active| active.options != options)
    {
        *active = Some(Live::open(options)?);
    }
    let frame = active.as_ref().unwrap().frames.frame();
    if frame.is_err() {
        *active = None;
    }
    frame
}

/// Pre-ScreenCaptureKit runtime compatibility. Supported macOS builds use SCStream.
fn legacy_display(display: u32) -> Result<Rgba, String> {
    let image = CGDisplay::new(display)
        .image()
        .ok_or_else(|| NO_DISPLAY.to_owned())?;
    let width = image.width() as u32;
    let height = image.height() as u32;
    let stride = image.bytes_per_row();
    let data = image.data();
    let bytes = data.bytes();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * stride;
        let row_bytes = bytes
            .get(start..start + width as usize * 4)
            .ok_or_else(|| "The screen image was short.".to_owned())?;
        pixels.extend_from_slice(row_bytes);
    }
    Ok(Rgba {
        width,
        height,
        pixels,
        bgra: true,
    })
}
