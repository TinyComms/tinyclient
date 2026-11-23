//! Windows loopback. A window is that process's tree (`ActivateAudioInterfaceAsync`). A screen
//! is the system mix with this process left out. Either falls back to the render device's
//! loopback, and the caller still subtracts what we just played.

use super::{cpal_input, Pcm, Pull, FRAME_SAMPLES};
use cpal::traits::{DeviceTrait, HostTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::{implement, Interface};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, StructuredStorage::PROPVARIANT, COINIT_MULTITHREADED, BLOB};
use windows::Win32::System::Variant::VT_BLOB;

const AUTOCONVERT: u32 = 0x8000_0000;
const SRC_QUALITY: u32 = 0x0800_0000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

enum Target {
    Include(u32),
    Exclude(u32),
    System,
}

pub(super) fn open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    match target(screen) {
        Target::Include(pid) => process(pid, false).or_else(|err| {
            tracing::debug!("mediad: process loopback did not open: {err}");
            system()
        }),
        Target::Exclude(pid) => process(pid, true).or_else(|err| {
            tracing::debug!("mediad: process loopback did not open: {err}");
            system()
        }),
        Target::System => system(),
    }
}

fn target(screen: Option<&str>) -> Target {
    let Some(id) = screen else {
        return Target::Exclude(std::process::id());
    };
    // A window of our own process cannot be looped back on its own: that tree is the call.
    match window_pid(id) {
        Some(pid) if pid != std::process::id() => Target::Include(pid),
        Some(_) => Target::System,
        None => Target::Exclude(std::process::id()),
    }
}

fn window_pid(id: &str) -> Option<u32> {
    let raw = id.strip_prefix("window:")?.parse::<isize>().ok()?;
    let mut pid = 0u32;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(raw as *mut core::ffi::c_void, &mut pid);
    }
    (pid != 0).then_some(pid)
}

fn system() -> Result<Box<dyn Pull>, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| "No speaker to share sound from.".to_owned())?;
    // The output format, opened as an input: WASAPI sets the loopback flag for a render device.
    let supported = device.default_output_config().map_err(|err| err.to_string())?;
    cpal_input(&device, supported).map(|device| Box::new(device) as Box<dyn Pull>)
}

struct Process {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pcm: Pcm,
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Pull for Process {
    fn pull(&mut self) -> Option<[i16; FRAME_SAMPLES]> {
        self.pcm.pull()
    }
}

enum Boot {
    Up,
    Down(String),
}

fn process(pid: u32, exclude: bool) -> Result<Box<dyn Pull>, String> {
    let (pcm, incoming) = Pcm::new(2, crate::RATE);
    let stop = Arc::new(AtomicBool::new(false));
    let (send, receive) = mpsc::channel();
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("share-audio".into())
        .spawn(move || {
            let err = unsafe { capture(pid, exclude, &incoming, &thread_stop, &send) };
            if let Err(err) = err {
                let _ = send.send(Boot::Down(err));
            }
        })
        .map_err(|err| err.to_string())?;
    match receive.recv_timeout(Duration::from_secs(3)) {
        Ok(Boot::Up) => Ok(Box::new(Process { stop, thread: Some(thread), pcm })),
        Ok(Boot::Down(err)) => {
            let _ = thread.join();
            Err(err)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            Err("Process loopback did not start.".to_owned())
        }
    }
}

unsafe fn capture(pid: u32, exclude: bool, incoming: &Mutex<VecDeque<i16>>, stop: &AtomicBool, boot: &mpsc::Sender<Boot>) -> Result<(), String> {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() {
        return Err(format!("Audio setup failed ({hr:?})."));
    }
    let result = unsafe { capture_started(pid, exclude, incoming, stop, boot) };
    unsafe { CoUninitialize() };
    result
}

unsafe fn capture_started(pid: u32, exclude: bool, incoming: &Mutex<VecDeque<i16>>, stop: &AtomicBool, boot: &mpsc::Sender<Boot>) -> Result<(), String> {
    let client = activate(pid, exclude)?;
    let format = float_48k();
    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUTOCONVERT | SRC_QUALITY;
    // 48 kHz stereo float, or the render-device fallback. A mix format would not match the pull.
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None) }.map_err(|err| err.to_string())?;
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|err| err.to_string())?;
    unsafe { client.Start() }.map_err(|err| err.to_string())?;
    let _ = boot.send(Boot::Up);
    while !stop.load(Ordering::Relaxed) {
        let packets = unsafe { capture.GetNextPacketSize() }.unwrap_or(0);
        if packets == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let mut data = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        if unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }.is_err() {
            break;
        }
        // Silent flag is 2. A silent packet is not the shared sound.
        if flags & 2 == 0 {
            push_packet(data, frames, incoming);
        }
        if unsafe { capture.ReleaseBuffer(frames) }.is_err() {
            break;
        }
    }
    let _ = unsafe { client.Stop() };
    Ok(())
}

fn float_48k() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: 2,
        nSamplesPerSec: crate::RATE,
        nAvgBytesPerSec: crate::RATE * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

fn push_packet(data: *const u8, frames: u32, incoming: &Mutex<VecDeque<i16>>) {
    if data.is_null() || frames == 0 {
        return;
    }
    let count = frames as usize * 2;
    let mut guard = incoming.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for i in 0..count {
        let sample = unsafe { data.cast::<f32>().add(i).read_unaligned() };
        guard.push_back((sample.clamp(-1.0, 1.0) * 32767.0) as i16);
    }
    let most = crate::RATE as usize * 2;
    while guard.len() > most {
        guard.pop_front();
    }
}

struct Slot {
    client: Mutex<Option<Result<*mut core::ffi::c_void, String>>>,
    ready: std::sync::Condvar,
    set: Mutex<bool>,
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Handler(Arc<Slot>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Handler_Impl {
    fn ActivateCompleted(&self, operation: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let operation = operation.ok()?;
        let taken = unsafe {
            let mut hr = windows::core::HRESULT(0);
            let mut unknown = None;
            operation.GetActivateResult(&mut hr, &mut unknown)?;
            if hr.is_err() {
                Err(hr.to_string())
            } else {
                match unknown.and_then(|unknown| unknown.cast::<IAudioClient>().ok()) {
                    Some(client) => Ok(Interface::into_raw(client)),
                    None => Err("Process loopback returned no audio client.".to_owned()),
                }
            }
        };
        let slot = &self.0;
        *slot.client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(taken);
        *slot.set.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        slot.ready.notify_one();
        Ok(())
    }
}

fn activate(pid: u32, exclude: bool) -> Result<IAudioClient, String> {
    let mode = if exclude { PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE } else { PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE };
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: mode },
        },
    };
    let blob = BLOB { cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: (&mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast() };
    let property = PROPVARIANT {
        Anonymous: windows::Win32::System::Com::StructuredStorage::PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(windows::Win32::System::Com::StructuredStorage::PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: windows::Win32::System::Com::StructuredStorage::PROPVARIANT_0_0_0 { blob },
            }),
        },
    };
    let slot = Arc::new(Slot { client: Mutex::new(None), ready: std::sync::Condvar::new(), set: Mutex::new(false) });
    let handler: IActivateAudioInterfaceCompletionHandler = Handler(slot.clone()).into();
    let _operation = unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(std::ptr::from_ref(&property)), &handler)
    }
    .map_err(|err| err.to_string())?;
    let guard = slot.set.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (guard, waited) = slot.ready.wait_timeout_while(guard, Duration::from_secs(2), |set| !*set).unwrap_or_else(|poisoned| poisoned.into_inner());
    if waited.timed_out() || !*guard {
        return Err("Process loopback did not answer.".to_owned());
    }
    drop(guard);
    let raw = slot.client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take().ok_or_else(|| "Process loopback did not answer.".to_owned())??;
    Ok(unsafe { Interface::from_raw(raw) })
}
