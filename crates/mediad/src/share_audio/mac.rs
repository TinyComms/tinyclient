//! macOS loopback through ScreenCaptureKit. The stream excludes this process's audio, which is
//! the call. Playback is still subtracted by the caller, in case a device mixes it back in.

use super::{Pcm, Pull, FRAME_SAMPLES};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSError, NSObject};
use objc2_screen_capture_kit::{SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const PERMISSION: &str = "Allow tinyclient in System Settings › Privacy & Security › Screen & System Audio Recording, then restart tinyclient.";

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(item: *const c_void);
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    buffers: u32,
    list: [AudioBuffer; 1],
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample: *const c_void,
        needed: *mut usize,
        list: *mut AudioBufferList,
        list_size: usize,
        structure: *const c_void,
        block: *const c_void,
        flags: u32,
        retained: *mut *mut c_void,
    ) -> i32;
}

pub(super) fn open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    if AnyClass::get(c"SCStream").is_none() {
        return Err("ScreenCaptureKit is not available on this Mac.".to_owned());
    }
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return Err(PERMISSION.to_owned());
    }
    let filter = crate::picture::mac_screen::share_filter(screen)?;
    Live::start(filter)
}

struct Live {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
    pcm: Pcm,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Pull for Live {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        self.pcm.pull()
    }
}

impl Live {
    fn start(filter: Retained<objc2_screen_capture_kit::SCContentFilter>) -> Result<Box<dyn Pull>, String> {
        let (pcm, incoming) = Pcm::new(2, crate::RATE);
        let (stop, stopped) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("share-audio".into())
            .spawn(move || {
                if let Err(err) = run(filter, incoming, stopped) {
                    tracing::warn!("mediad: share audio: {err}");
                }
            })
            .map_err(|err| err.to_string())?;
        Ok(Box::new(Live { stop, thread: Some(thread), pcm }))
    }
}

fn run(filter: Retained<objc2_screen_capture_kit::SCContentFilter>, incoming: Arc<Mutex<VecDeque<i16>>>, stop: std::sync::mpsc::Receiver<()>) -> Result<(), String> {
    let output = Output::alloc().set_ivars(incoming);
    let output: Retained<Output> = unsafe { msg_send![super(output), init] };
    let queue = DispatchQueue::new("dev.tinyclient.share.audio", None);
    let stream = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(2);
        config.setHeight(2);
        config.setCapturesAudio(true);
        config.setSampleRate(48_000);
        config.setChannelCount(2);
        config.setExcludesCurrentProcessAudio(true);
        let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(ProtocolObject::from_ref(&*output)));
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Audio, Some(&queue))
            .map_err(|err| err.localizedDescription().to_string())?;
        stream
    };
    let started = wait(|done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) });
    if let Err(err) = started {
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
        return Err(err);
    }
    let _ = stop.recv();
    unsafe { stream.stopCaptureWithCompletionHandler(None) };
    Ok(())
}

fn wait(start: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<(), String> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let done = RcBlock::new(move |error: *mut NSError| {
        let result = unsafe { error.as_ref() }.map_or(Ok(()), |error| Err(error.localizedDescription().to_string()));
        let _ = send.try_send(result);
    });
    start(&done);
    receive.recv_timeout(Duration::from_secs(4)).map_err(|_| "ScreenCaptureKit did not respond.".to_owned())?
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TinyMediadShareAudio"]
    #[ivars = Arc<Mutex<VecDeque<i16>>>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamDelegate for Output {}

    unsafe impl SCStreamOutput for Output {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(&self, _: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Audio {
                take_audio(sample, self.ivars());
            }
        }
    }
);

fn take_audio(sample: &CMSampleBuffer, incoming: &Mutex<VecDeque<i16>>) {
    let mut list = AudioBufferList { buffers: 1, list: [AudioBuffer { channels: 0, bytes: 0, data: std::ptr::null_mut() }] };
    let mut retained = std::ptr::null_mut();
    let status = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample as *const CMSampleBuffer as *const c_void,
            std::ptr::null_mut(),
            &mut list,
            std::mem::size_of::<AudioBufferList>(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut retained,
        )
    };
    if status == 0 && !list.list[0].data.is_null() && list.list[0].bytes >= 4 {
        let samples = list.list[0].bytes as usize / 4;
        let data = list.list[0].data.cast::<f32>();
        let mut guard = incoming.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for i in 0..samples {
            let sample = unsafe { data.add(i).read_unaligned() };
            guard.push_back((sample.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let most = crate::RATE as usize * 2;
        while guard.len() > most {
            guard.pop_front();
        }
    }
    if !retained.is_null() {
        unsafe { CFRelease(retained) };
    }
}
