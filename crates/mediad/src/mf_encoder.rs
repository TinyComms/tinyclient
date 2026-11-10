//! The Windows Media Foundation H.264 encoder, the machine's own hardware ahead of the
//! OpenH264 download (`picture/desktop`), which stays as the fallback.
//!
//! A hardware encoder is an asynchronous Media Foundation transform (MFT): it is found with
//! `MFTEnumEx` over the registered hardware encoders, woken by `MF_TRANSFORM_ASYNC_UNLOCK`,
//! fed NV12 frames through `ProcessInput`, and drained through its event queue
//! (`METransformNeedInput`, `METransformHaveOutput`) with `ProcessOutput`. The output type
//! is negotiated first, at the constrained baseline the SFU takes, and what the transform
//! kept is read back and held to that: a driver that would put Main or High on the wire,
//! or one that names no profile at all, is refused in its own words and OpenH264 takes the
//! job. The rate is CBR at the bitrate the track asked for (`MF_MT_AVG_BITRATE` and
//! `CODECAPI_AVEncCommonRateControlMode`), a keyframe is asked for with
//! `CODECAPI_AVEncVideoForceKeyFrame`, a frame is a keyframe only when its sample says
//! `MFSampleExtension_CleanPoint`, and the output is normalized to the Annex B byte stream
//! the SFU and the OpenH264 decoder take. A frame whose bits are not out yet comes back as
//! an empty [`Encoded`], which the share already treats as a skip; they leave on a later
//! frame's pump. The first frame may wait the encoder's warmup for its bits; every later
//! one waits a far shorter moment, so a driver that holds frames back never stalls the
//! capture thread.
//!
//! The asks of the transform are narrowed to the [`Mft`] trait, and the transform comes
//! from a [`Source`]: [`Hardware`] is the machine's own, and the tests hold fakes against
//! the same words, which is how the open, the profile refusal, the keyframe flag and the
//! frame loop are proven without hardware.
//!
//! Everything here runs where the encoder was opened: the share's or camera's capture
//! thread, COM in the Multi Threaded Apartment, Media Foundation started without its
//! sockets. Both are taken up by [`platform`] and let go by the [`Platform`] guard the
//! encoder holds, after the transform is. `Outgoing` only ever moves between threads before
//! an encoder exists, and MTA objects are callable from every thread of the process, so the
//! `Send` on [`Encoder`] is honest. What the encoder keeps between frames is its bitrate,
//! its frame rate, and whether it has an input slot to fill; a change of frame size is not
//! a tune but a new encoder (`coded` in `desktop`), as with OpenH264.

use std::ptr::null_mut;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, VARIANT_BOOL};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonLowLatency, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    IMFActivate, ICodecAPI, IMFMediaEventGenerator, IMFMediaType, IMFTransform, MFVideoInterlace_Progressive,
    MF_E_NO_EVENTS_AVAILABLE, MF_EVENT_FLAG_NO_WAIT, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE,
    MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFShutdown, METransformHaveOutput,
    METransformNeedInput, MFTEnumEx, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_REGISTER_TYPE_INFO, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFStartup, MFSTARTUP_NOSOCKET, MFVideoFormat_H264, MFVideoFormat_NV12, MFSampleExtension_CleanPoint,
    eAVEncCommonRateControlMode_CBR, eAVEncH264VLevel3_1, eAVEncH264VLevel5_1, eAVEncH264VProfile_Base,
    eAVEncH264VProfile_ConstrainedBase,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Variant::{VT_BOOL, VT_UI4, VARIANT, VariantInit};

use crate::picture::{Ask, Encoded, Planes};

/// How long the first frame waits for the machine's encoder to answer, which is the one
/// frame that may cover a driver's warmup. Hardware encoders in low latency answer inside
/// a frame's time; when one does not, the frame goes out empty and its bits leave on the
/// next pump, the way a skip already does.
const ANSWER_WAIT: Duration = Duration::from_millis(120);
/// How long every frame after the first waits for its own bits: far less than a frame's
/// time, so a slow driver or a lookahead holds the capture thread no longer than this.
const OUTPUT_WAIT: Duration = Duration::from_millis(10);
/// How often the event queue is looked at while waiting for that answer.
const POLL: Duration = Duration::from_millis(1);
/// The NV12 stride promised to the encoder, the width rounded up as the hardware wants it.
const STRIDE_ALIGN: usize = 16;
/// The profiles the SFU is told to expect, as Media Foundation names them:
/// `eAVEncH264VProfile_Base` (66, plain baseline) and `eAVEncH264VProfile_ConstrainedBase`
/// (256). Main (77) and High (100) are refused, and so is a type that names none.
const BASELINE: u32 = eAVEncH264VProfile_Base.0 as u32;
const CONSTRAINED_BASELINE: u32 = eAVEncH264VProfile_ConstrainedBase.0 as u32;

/// Which backend a track's encoder takes: the machine's hardware on a clean probe, the
/// OpenH264 download on any failure. The one place that decision lives, so tests can hold
/// it against a probe result without hardware or the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    MediaFoundation,
    OpenH264,
}

/// The backend a probe result means.
pub fn choose(probe: &Result<(), String>) -> Choice {
    if probe.is_ok() { Choice::MediaFoundation } else { Choice::OpenH264 }
}

/// Whether this machine has a hardware H.264 encoder Media Foundation can drive, asked once
/// and remembered for the process: the share and camera paths read this to keep the
/// OpenH264 download off a machine that will never need it, and [`Encoder::open`] asks the
/// system again before it opens. The probe runs on a thread of its own, so the answer is
/// the machine's and not the asking thread's apartment; an answer that was never really
/// taken (the probe's thread would not start) is not remembered and is asked again at the
/// next open.
pub fn available() -> Result<(), String> {
    static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
    match probe_thread() {
        Ok(answer) => AVAILABLE.get_or_init(|| answer).clone(),
        Err(why) => Err(why),
    }
}

/// The probe on a thread of its own. A fresh thread holds no COM apartment, so the probe's
/// own `CoInitializeEx` decides how it runs and an `RPC_E_CHANGED_MODE` from elsewhere
/// cannot answer for the machine. The outer `Err` is the thread's, not the machine's, and
/// is never cached.
fn probe_thread() -> Result<Result<(), String>, String> {
    let asked = std::thread::Builder::new().name("mediad-mf-probe".into()).spawn(probe)
        .map_err(|err| format!("the encoder probe's thread would not start: {err}"))?;
    match asked.join() {
        Ok(answer) => Ok(answer),
        Err(_) => Err("the encoder probe stopped unexpectedly.".to_owned()),
    }
}

/// One look at what the machine lists, without opening anything. Fails in its own words: a
/// machine with no hardware MFT shares through OpenH264, and so does one whose COM or Media
/// Foundation would not start.
fn probe() -> Result<(), String> {
    let _platform = platform()?;
    if enumerate() == 0 {
        return Err("This machine lists no hardware H.264 encoder.".to_owned());
    }
    Ok(())
}

/// How many hardware H.264 encoders this machine lists, taking NV12 and answering H.264.
/// Every activate the listing handed out is let go here.
fn enumerate() -> u32 {
    let (list, count) = match enum_mfts() {
        Ok(found) => found,
        Err(_) => (null_mut(), 0),
    };
    // SAFETY: no entry is kept: every activate in the array is released, then the array.
    unsafe { let _ = release(list, count, None); }
    count
}

/// The hardware H.264 encoders MFTEnumEx lists, as the raw array it handed back and its
/// count. A failed listing is an empty one: the answer is the same machine with none.
fn enum_mfts() -> Result<(*mut Option<IMFActivate>, u32), windows::core::Error> {
    let takes = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let answers = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut list: *mut Option<IMFActivate> = null_mut();
    let mut count = 0u32;
    // SAFETY: both registration types outlive the call, and the list it hands back is
    // freed whole by `release`, after the entries are let go one by one. Nothing here
    // keeps a handle into it.
    unsafe { MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, Some(&takes), Some(&answers), &mut list, &mut count) }?;
    Ok((list, count))
}

/// Every activate in MFTEnumEx's array let go, then the array itself. Which entry `keep`
/// moves out first is the caller's one take, handed back; the rest are released whatever
/// happens with it.
///
/// SAFETY: `list` is MFTEnumEx's own allocation holding `count` entries (or null with no
/// count), and no other reference into it exists.
unsafe fn release(list: *mut Option<IMFActivate>, count: u32, keep: Option<usize>) -> Option<IMFActivate> {
    let mut kept = None;
    for at in 0..count as usize {
        let entry = list.add(at);
        if keep == Some(at) {
            // SAFETY: the entry is read once, moved out of the array before it goes.
            kept = unsafe { std::ptr::read(entry) };
        } else {
            // SAFETY: the entry is MFTEnumEx's own, dropped here to release the activate.
            unsafe { std::ptr::drop_in_place(entry) };
        }
    }
    // SAFETY: the array is MFTEnumEx's own allocation, freed whole.
    unsafe { CoTaskMemFree(Some(list.cast::<core::ffi::c_void>())) };
    kept
}

/// COM in the Multi Threaded Apartment and Media Foundation, taken up for one encoder or
/// one probe and let go when the guard goes, each undone only if this guard took it up.
/// Media Foundation is per process and keeps count, so an encoder closing beside another
/// still running leaves that one alone; the apartment is per thread and keeps count the
/// same way. The guard must drop on the thread `platform` ran on, which is the encoder's
/// whole life (the share's or the camera's capture thread), and drops after the encoder's
/// transform, which is the field before it, so nothing is asked of Media Foundation after
/// it let go.
struct Platform {
    /// Whether this guard's thread was brought into the MTA by [`platform`].
    com: bool,
    /// Whether this guard started Media Foundation.
    mf: bool,
}

/// COM in the Multi Threaded Apartment, per thread, and Media Foundation, per process.
/// Both keep count, so an encoder thread that meets another's work answers at once.
fn platform() -> Result<Platform, String> {
    // SAFETY: the apartment call is the documented per-thread init; its result is checked.
    let started = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if started.is_err() {
        return Err(if started == RPC_E_CHANGED_MODE {
            "This thread already holds a COM apartment the encoder cannot share.".to_owned()
        } else {
            format!("COM would not start for the encoder: {started}")
        });
    }
    let mut guard = Platform { com: true, mf: false };
    // The init is undone by the guard when this open ends, on the thread that took it.
    // SAFETY: the version and the no-sockets flag are the constants Media Foundation reads.
    if let Err(err) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) } {
        return Err(format!("Media Foundation would not start: {err}"));
    }
    guard.mf = true;
    Ok(guard)
}

impl Drop for Platform {
    fn drop(&mut self) {
        if self.mf {
            // SAFETY: Media Foundation was started by this guard's platform call.
            unsafe { let _ = MFShutdown(); }
        }
        if self.com {
            // SAFETY: the apartment was taken by this guard's platform call, on the thread
            // the guard is dropped on.
            unsafe { CoUninitialize() }
        }
    }
}

#[cfg(test)]
impl Platform {
    /// A guard for nothing started, so the tests assemble an encoder without touching the
    /// machine's COM, and the drop has nothing to undo.
    fn unstarted() -> Platform {
        Platform { com: false, mf: false }
    }
}

/// What the encoder's event queue said: an input slot opened, or a frame is out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    NeedInput,
    HaveOutput,
}

/// One frame out of the transform: its bytes as the Annex B stream, and what the sample
/// said about being a clean point, if it said anything.
#[derive(Debug, Clone, PartialEq)]
struct Taken {
    bytes: Vec<u8>,
    clean: Option<u32>,
}

/// A frame is a keyframe only when the sample says it is a clean point, and says it
/// loudly: an attribute that is merely there, or there as zero, is a delta frame.
fn clean_point_is_key(clean: Option<u32>) -> bool {
    clean.unwrap_or(0) != 0
}

/// The one hardware transform an [`Encoder`] drives, narrowed to the words open and encode
/// ask of it. The real one is the machine's MFT in COM, handed over by [`Hardware`]; the
/// tests hold fakes against the same words, which is how the open, the fallback and the
/// keyframe flag are held without hardware.
trait Mft: Send {
    /// Wake the async transform for event-driven use, before any type is set.
    fn unlock(&mut self) -> Result<(), String>;
    /// The output ask: H.264 at the bitrate, size and rate of the track, constrained
    /// baseline at the level the track asked for. Answers the profile of the type the
    /// transform kept, so what goes on the wire can be held to what was promised; `None`
    /// is a kept type that names no profile.
    fn set_output_type(&mut self, width: usize, height: usize, bps: u32, fps: f32, screen: bool) -> Result<Option<u32>, String>;
    /// The input ask: NV12 at the promised stride, whole frames, at the size and rate.
    fn set_input_type(&mut self, width: usize, height: usize, stride: usize, frame: usize, fps: f32) -> Result<(), String>;
    /// The codec properties settled for the stream: CBR at `bps`, low latency, no B
    /// pictures. A refusal is said in the log and lived with, never a failure.
    fn configure(&mut self, bps: u32);
    /// Streaming begun.
    fn begin(&mut self) -> Result<(), String>;
    /// The next event that asks something of the encoder, skipping the kinds a share needs
    /// nothing of; `None` when the queue has nothing more now.
    fn event(&mut self) -> Result<Option<Event>, String>;
    /// One NV12 frame in, at `at_100ns` since the stream began and lasting
    /// `lasting_100ns`.
    fn give(&mut self, planes: &Planes, stride: usize, frame: usize, at_100ns: i64, lasting_100ns: i64) -> Result<(), String>;
    /// One frame out, or `None` when the transform has nothing out after all.
    fn take(&mut self) -> Result<Option<Taken>, String>;
    /// The bitrate moved; answers whether the transform took it.
    fn tune(&mut self, bps: u32) -> bool;
    /// A keyframe for the next frame in.
    fn force_key(&mut self);
    /// Flush and end streaming, at close.
    fn end(&mut self);
}

/// Where an encoder's transform comes from: [`Hardware`], the machine's first hardware MFT
/// that takes NV12 and answers H.264, and the tests' fakes.
trait Source {
    fn activate(&self) -> Result<Box<dyn Mft>, String>;
}

/// The machine's own encoders, as MFTEnumEx lists them: the first hardware MFT that takes
/// NV12 and answers H.264, activated, shut down in the activate's own hands, with every
/// activate the listing handed out let go.
struct Hardware;

impl Source for Hardware {
    fn activate(&self) -> Result<Box<dyn Mft>, String> {
        let (list, count) = enum_mfts().map_err(|err| format!("the hardware encoders could not be listed: {err}"))?;
        // SAFETY: entry 0 is taken out for this encoder; every other entry and the array
        // itself are released here, whatever happens below.
        let chosen = unsafe { release(list, count, Some(0)) };
        let Some(activate) = chosen else {
            return Err("This machine lists no hardware H.264 encoder.".to_owned());
        };
        // SAFETY: the activation object hands out the transform it stands for, once.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .map_err(|err| format!("the hardware encoder could not be started: {err}"))?;
        // The activate need not keep the encoder's setup alive once the transform is out:
        // its shutdown hands the lifetime to the transform this encoder holds, the way
        // MFTEnumEx's own contract asks.
        // SAFETY: the activate is still owned here.
        unsafe { let _ = activate.ShutdownObject(); }
        drop(activate);
        // Both casts are on the transform this open created; `cast` is safe in
        // windows-core: it cannot name a null or wrong-vtable object without an `Err`.
        let events: IMFMediaEventGenerator = transform.cast()
            .map_err(|err| format!("the hardware encoder has no event queue: {err}"))?;
        // `codec` is `None` when the MFT exposes no codec properties, which leaves a
        // keyframe ask and a bitrate move without their handle; the picture still encodes.
        let codec: Option<ICodecAPI> = transform.cast().ok();
        Ok(Box::new(RealMft { transform, events, codec }))
    }
}

/// The machine's encoder as COM hands it over: the async MFT, its event queue, and the
/// codec properties that survive a frame.
struct RealMft {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
}

/// SAFETY: the MFT is opened, driven, and dropped on one thread (the share's or the
/// camera's capture thread); `Outgoing` moves only before any encoder exists. The COM
/// apartment is the MTA and Media Foundation objects answer from every thread of the
/// process, so even a move after the fact would stay within what the platform allows.
unsafe impl Send for RealMft {}

impl Mft for RealMft {
    fn unlock(&mut self) -> Result<(), String> {
        // SAFETY: the attributes belong to the transform this encoder holds.
        unsafe {
            let attributes = self.transform.GetAttributes()
                .map_err(|err| format!("the hardware encoder told nothing about itself: {err}"))?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(|err| format!("the hardware encoder could not be unlocked: {err}"))
        }
    }

    fn set_output_type(&mut self, width: usize, height: usize, bps: u32, fps: f32, screen: bool) -> Result<Option<u32>, String> {
        // SAFETY: the media type is built here and set on the transform this encoder
        // holds; what the transform kept is read back before the call returns.
        unsafe {
            let output = output_type(width, height, bps, fps, screen)?;
            self.transform.SetOutputType(0, &output, 0)
                .map_err(|err| format!("the hardware encoder would not take an H.264 output: {err}"))?;
            let kept = self.transform.GetOutputCurrentType(0)
                .map_err(|err| format!("the hardware encoder would not say what it kept of the H.264 output: {err}"))?;
            Ok(kept.GetUINT32(&MF_MT_MPEG2_PROFILE).ok())
        }
    }

    fn set_input_type(&mut self, width: usize, height: usize, stride: usize, frame: usize, fps: f32) -> Result<(), String> {
        // SAFETY: the media type is created here and set before it drops.
        unsafe {
            let input = input_type(width, height, stride, frame, fps)?;
            self.transform.SetInputType(0, &input, 0)
                .map_err(|err| format!("the hardware encoder would not take NV12: {err}"))
        }
    }

    fn configure(&mut self, bps: u32) {
        let Some(codec) = &self.codec else {
            return;
        };
        // Both rates are said twice on purpose: once on the output type, which the
        // MFT reads as it opens, and once through the codec properties, which the
        // rate control holds for the frames after. A refusal is logged and lived
        // with: the encoder still answers, at a rate of its own choosing.
        // SAFETY: each property is read through the codec handle this encoder holds, and
        // each variant outlives the call it is handed to.
        unsafe {
            if let Err(err) = codec.SetValue(&CODECAPI_AVEncCommonRateControlMode, &counted(eAVEncCommonRateControlMode_CBR.0 as u32)) {
                tracing::debug!("mediad: the hardware encoder took no CBR rate control: {err}");
            }
            if let Err(err) = codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &counted(bps)) {
                tracing::debug!("mediad: the hardware encoder took no mean bitrate: {err}");
            }
            if let Err(err) = codec.SetValue(&CODECAPI_AVEncCommonLowLatency, &yes()) {
                tracing::debug!("mediad: the hardware encoder took no low latency switch: {err}");
            }
            if let Err(err) = codec.SetValue(&CODECAPI_AVLowLatencyMode, &yes()) {
                tracing::debug!("mediad: the hardware encoder took no low latency mode: {err}");
            }
            if let Err(err) = codec.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &counted(0)) {
                tracing::debug!("mediad: the hardware encoder took no B picture count: {err}");
            }
        }
    }

    fn begin(&mut self) -> Result<(), String> {
        // SAFETY: the transform is owned here; both messages are its own.
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|err| format!("the hardware encoder would not start streaming: {err}"))?;
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|err| format!("the hardware encoder would not start streaming: {err}"))
        }
    }

    fn event(&mut self) -> Result<Option<Event>, String> {
        loop {
            // SAFETY: the event generator belongs to this encoder and is not held
            // elsewhere; each event is this call's own, read once and dropped.
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(None),
                Err(err) => return Err(format!("the hardware encoder's events failed: {err}")),
            };
            let kind = unsafe { event.GetType() }.map_err(|err| format!("the hardware encoder's events could not be read: {err}"))?;
            if kind == METransformHaveOutput.0 as u32 {
                return Ok(Some(Event::HaveOutput));
            }
            if kind == METransformNeedInput.0 as u32 {
                return Ok(Some(Event::NeedInput));
            }
            // METransformDrainComplete and METransformInputStreamStateChanged need nothing
            // from a share: nothing is ever drained, and the input state is read back
            // through `wants_input` alone.
        }
    }

    fn give(&mut self, planes: &Planes, stride: usize, frame: usize, at_100ns: i64, lasting_100ns: i64) -> Result<(), String> {
        // SAFETY: the sample is created here, filled here, and handed to the transform,
        // which owns its reference from `ProcessInput` on; the buffer pointer is used
        // within its lock below and gone before the lock is let go.
        unsafe {
            let buffer = MFCreateMemoryBuffer(frame as u32).map_err(|err| format!("the frame could not be given room: {err}"))?;
            let mut data: *mut u8 = null_mut();
            buffer.Lock(&mut data, None, None).map_err(|err| format!("the frame could not be written: {err}"))?;
            nv12(planes, stride, std::slice::from_raw_parts_mut(data, frame));
            let _ = buffer.SetCurrentLength(frame as u32);
            buffer.Unlock().map_err(|err| format!("the frame could not be handed over: {err}"))?;

            let sample = MFCreateSample().map_err(|err| format!("the frame could not be wrapped: {err}"))?;
            sample.AddBuffer(&buffer).map_err(|err| format!("the frame could not be wrapped: {err}"))?;
            sample.SetSampleTime(at_100ns).map_err(|err| format!("the frame's time was refused: {err}"))?;
            sample.SetSampleDuration(lasting_100ns.max(1)).map_err(|err| format!("the frame's length was refused: {err}"))?;
            self.transform.ProcessInput(0, &sample, 0).map_err(|err| format!("the hardware encoder would not take the frame: {err}"))?;
            Ok(())
        }
    }

    fn take(&mut self) -> Result<Option<Taken>, String> {
        // SAFETY: the output slot belongs to this call; the sample the transform fills is
        // taken out of it and read before it is dropped here.
        unsafe {
            let mut slot = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            self.transform.ProcessOutput(0, &mut slot, &mut status).map_err(|err| format!("the hardware encoder gave no frame: {err}"))?;
            let Some(sample) = std::mem::ManuallyDrop::take(&mut slot[0].pSample) else {
                return Ok(None);
            };
            // The attribute's value is what a keyframe is, not its existence: a delta
            // frame carries the extension set to zero.
            let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).ok();
            // The sample's buffers are made one here, whatever count the encoder used.
            let contiguous = sample.ConvertToContiguousBuffer().map_err(|err| format!("the encoded frame could not be read: {err}"))?;
            let mut data: *mut u8 = null_mut();
            let mut length = 0u32;
            contiguous.Lock(&mut data, None, Some(&mut length)).map_err(|err| format!("the encoded frame could not be read: {err}"))?;
            let bytes = annex_b(std::slice::from_raw_parts(data, length as usize));
            contiguous.Unlock().map_err(|err| format!("the encoded frame could not be let go: {err}"))?;
            Ok(Some(Taken { bytes, clean }))
        }
    }

    fn tune(&mut self, bps: u32) -> bool {
        let Some(codec) = &self.codec else {
            return false;
        };
        // SAFETY: the property is read through the codec handle this encoder holds, and
        // the variant outlives the call.
        match unsafe { codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &counted(bps)) } {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("mediad: the hardware encoder refused a bitrate of {}: {err}", bps);
                false
            }
        }
    }

    fn force_key(&mut self) {
        let Some(codec) = &self.codec else {
            return;
        };
        // The property carries no value; setting it is the ask. An encoder without codec
        // properties keeps its own keyframe pace.
        // SAFETY: as in `tune`, with a property whose value is ignored.
        if let Err(err) = unsafe { codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &counted(0)) } {
            tracing::debug!("mediad: the hardware encoder refused a keyframe: {err}");
        }
    }

    fn end(&mut self) {
        // SAFETY: the transform is still owned here; both messages are its own.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// The machine's hardware H.264 encoder, held as the async MFT behind the [`Mft`] trait,
/// the frame's shape, and what the track asked for. It is opened, driven, and dropped on
/// one thread (the share's or the camera's capture thread); COM and Media Foundation live
/// in the [`Platform`] guard, let go after the transform is.
pub struct Encoder {
    mft: Box<dyn Mft>,
    width: usize,
    height: usize,
    /// The stride promised in the input type: the width rounded up as the hardware wants.
    stride: usize,
    /// One NV12 frame at that stride: the Y plane and the two interleaved chroma planes.
    frame: usize,
    fps: f32,
    /// What the encoder holds now, so a tune is only sent when the ask moved.
    bps: u32,
    /// Whether the encoder has an input slot open: a `METransformNeedInput` seen and not
    /// yet fed.
    wants_input: bool,
    /// Whether a frame has gone in yet: the first may wait the encoder's warmup for its
    /// bits, every later one waits at most [`OUTPUT_WAIT`].
    fed: bool,
    /// COM and Media Foundation, held so they outlive the transform and are let go after
    /// it (the field after the transform's own).
    _platform: Platform,
}

impl Encoder {
    /// The first hardware encoder that takes NV12 and answers H.264, set up for CBR at
    /// `bps` and at most `fps`, level 5.1 for a screen and 3.1 for a camera, at the
    /// constrained baseline the SFU takes. Its own words when there is no such encoder, or
    /// one would not take its setup, or one would put a profile other than the baseline on
    /// the wire; the caller falls back to OpenH264 on any of them.
    pub fn open(width: u32, height: u32, bps: u32, fps: f32, screen: bool) -> Result<Encoder, String> {
        Self::open_with(&Hardware, platform()?, width, height, bps, fps, screen)
    }

    /// The open, against any source of transforms. The output type is negotiated first, at
    /// the promised profile — the machine's encoders refuse an input type while the output
    /// type is unset — and what the transform kept is held to the promise before the input
    /// type, the codec properties and streaming follow. Nothing is retried without the
    /// profile: whatever else the driver picked would go out on the wire as a stream the
    /// SFU and the receivers were told never to expect, so such a driver is refused and
    /// OpenH264 takes the job.
    fn open_with(source: &dyn Source, platform: Platform, width: u32, height: u32, bps: u32, fps: f32, screen: bool) -> Result<Encoder, String> {
        let (width, height) = (width.max(2) as usize, height.max(2) as usize);
        let stride = (width + STRIDE_ALIGN - 1) & !(STRIDE_ALIGN - 1);
        let frame = stride * height * 3 / 2;

        let mut mft = source.activate()?;
        mft.unlock()?;
        let kept = mft.set_output_type(width, height, bps, fps, screen)?
            .ok_or_else(|| "the hardware encoder would not say which H.264 profile it encodes.".to_owned())?;
        if kept != BASELINE && kept != CONSTRAINED_BASELINE {
            return Err(format!("the hardware encoder encodes H.264 profile {kept}, not the baseline the SFU takes."));
        }
        mft.set_input_type(width, height, stride, frame, fps)?;
        mft.configure(bps);
        mft.begin()?;
        let mut encoder = Encoder { mft, width, height, stride, frame, fps, bps, wants_input: false, fed: false, _platform: platform };
        // The first input slot is usually open before the first frame; wait a moment
        // for it, so the first frame of a share is fed at once and not dropped.
        let mut nothing = Vec::new();
        let until = Instant::now() + ANSWER_WAIT;
        while !encoder.wants_input && Instant::now() < until {
            encoder.drain(&mut nothing)?;
            if !encoder.wants_input {
                std::thread::sleep(POLL);
            }
        }
        Ok(encoder)
    }

    /// One frame in, whatever is out. A frame whose bits are still on their way comes back
    /// empty, like a skip; an encoded frame comes back marked when it is a keyframe, so
    /// the share sends it as one. After the first frame is in, the wait for a frame's own
    /// bits is bounded well under a frame's time, so the capture thread is never held
    /// long.
    pub fn encode(&mut self, planes: &Planes, ask: &Ask) -> Result<Encoded, String> {
        if planes.width != self.width || planes.height != self.height {
            return Err(format!("The encoder was opened for {}×{} and handed {}×{}.", self.width, self.height, planes.width, planes.height));
        }
        if ask.bps != self.bps && ask.bps > 0 && self.mft.tune(ask.bps) {
            self.bps = ask.bps;
        }
        if ask.key {
            self.mft.force_key();
        }
        // What is already out, before this frame goes in: the machine may be a frame ahead.
        let mut bytes = Vec::new();
        let mut key = self.drain(&mut bytes)?;
        if self.wants_input {
            // The ask's clock is milliseconds; Media Foundation's is 100 nanoseconds.
            let at = (ask.at.as_millis() as i64).saturating_mul(10_000);
            let lasting = (10_000_000.0 / self.fps.max(1.0)) as i64;
            self.mft.give(planes, self.stride, self.frame, at, lasting)?;
            self.wants_input = false;
            let first = !self.fed;
            self.fed = true;
            // A bounded moment for this frame's own bits. The first frame may wait the
            // encoder's warmup; every later one waits no longer than [`OUTPUT_WAIT`], so a
            // driver holding frames back never stalls the capture thread. When the bits
            // are not out in time, the empty frame goes and the pump catches them next
            // time.
            let until = Instant::now() + if first { ANSWER_WAIT } else { OUTPUT_WAIT };
            while bytes.is_empty() && Instant::now() < until {
                key |= self.drain(&mut bytes)?;
                if bytes.is_empty() {
                    std::thread::sleep(POLL);
                }
            }
        }
        if bytes.is_empty() {
            key = false;
        }
        Ok(Encoded { bytes, key })
    }

    /// Everything the event queue holds now, without waiting: frames out, their bytes
    /// appended, and whether what came out holds a keyframe; the open input slot is
    /// remembered for the next frame in.
    fn drain(&mut self, bytes: &mut Vec<u8>) -> Result<bool, String> {
        let mut key = false;
        while let Some(event) = self.mft.event()? {
            match event {
                Event::HaveOutput => {
                    if let Some(taken) = self.mft.take()? {
                        if clean_point_is_key(taken.clean) {
                            key = true;
                        }
                        bytes.extend_from_slice(&taken.bytes);
                    }
                }
                Event::NeedInput => self.wants_input = true,
            }
        }
        Ok(key)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.mft.end();
    }
}

/// The input type the encoder is fed: NV12 at the promised stride, whole frames, at the
/// size and rate the track asked for.
fn input_type(width: usize, height: usize, stride: usize, frame: usize, fps: f32) -> Result<IMFMediaType, String> {
    // SAFETY: the media type is created here and returned; the caller sets it before drop.
    unsafe {
        let kind = MFCreateMediaType().map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT64(&MF_MT_FRAME_SIZE, size(width, height)).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT64(&MF_MT_FRAME_RATE, rate(fps)).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_SAMPLE_SIZE, frame as u32).map_err(|err| format!("the input type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride as u32).map_err(|err| format!("the input type could not be built: {err}"))?;
        Ok(kind)
    }
}

/// The output type asked for: H.264 at the bitrate, size and rate of the track, at the
/// constrained baseline the SFU takes. What the encoder actually kept is read back and
/// held to the promise by [`Encoder::open_with`]; there is no retry without the profile,
/// because whatever else the driver picked would go out on the wire as a stream the SFU
/// was told never to expect. A level holds the size and rate, 5.1 for a 1440p share at 60.
fn output_type(width: usize, height: usize, bps: u32, fps: f32, screen: bool) -> Result<IMFMediaType, String> {
    let level = if screen { eAVEncH264VLevel5_1 } else { eAVEncH264VLevel3_1 };
    // SAFETY: as in `input_type`, the type is returned to be set.
    unsafe {
        let kind = MFCreateMediaType().map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_AVG_BITRATE, bps).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT64(&MF_MT_FRAME_SIZE, size(width, height)).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT64(&MF_MT_FRAME_RATE, rate(fps)).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, size(1, 1)).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_ConstrainedBase.0 as u32).map_err(|err| format!("the output type could not be built: {err}"))?;
        kind.SetUINT32(&MF_MT_MPEG2_LEVEL, level.0 as u32).map_err(|err| format!("the output type could not be built: {err}"))?;
        Ok(kind)
    }
}

/// A frame size as Media Foundation packs it: width high, height low.
fn size(width: usize, height: usize) -> u64 {
    ((width as u64) << 32) | height as u64
}

/// A frame rate as Media Foundation packs it: thousandths of a frame a second over 1000.
fn rate(fps: f32) -> u64 {
    ((fps * 1000.).round().max(1.) as u64) << 32 | 1000
}

/// A `VARIANT` holding a count, which is how the codec properties are read.
fn counted(count: u32) -> VARIANT {
    // SAFETY: a VARIANT is initialized here and its body is filled before the property
    // call it is handed to reads it.
    unsafe {
        let mut value = VariantInit();
        // The body is a union behind a ManuallyDrop, which rustc does not walk on its
        // own: each layer is taken by hand.
        let body = &mut *value.Anonymous.Anonymous;
        body.vt = VT_UI4;
        body.Anonymous.ulVal = count;
        value
    }
}

/// A `VARIANT` holding TRUE, for the switches.
fn yes() -> VARIANT {
    // SAFETY: as in `counted`, with a boolean.
    unsafe {
        let mut value = VariantInit();
        let body = &mut *value.Anonymous.Anonymous;
        body.vt = VT_BOOL;
        body.Anonymous.boolVal = VARIANT_BOOL(-1);
        value
    }
}

/// `planes` as NV12 into `out`, Y rows at `stride` and the two chroma planes interleaved
/// under them. The padding rows and bytes stay what the buffer was made with, and a buffer
/// too short takes what fits and nothing past it. The phone's `convert::nv12` packs the
/// same planes tightly into a `VideoFrame`; this one is the shape the input type promises
/// the machine's encoder: its stride, in the buffer its sample was given.
fn nv12(planes: &Planes, stride: usize, out: &mut [u8]) {
    let (width, height) = (planes.width, planes.height);
    for row in 0..height {
        let at = row * stride;
        if at + width > out.len() {
            return;
        }
        out[at..at + width].copy_from_slice(&planes.y[row * width..(row + 1) * width]);
    }
    let chroma = width / 2;
    for row in 0..height / 2 {
        let at = height * stride + row * stride;
        for col in 0..chroma {
            let cell = at + col * 2;
            if cell + 1 >= out.len() {
                return;
            }
            let from = row * chroma + col;
            out[cell] = planes.u[from];
            out[cell + 1] = planes.v[from];
        }
    }
}

/// The Annex B byte stream the SFU and the decoder take: every NAL unit behind a start
/// code. Media Foundation's H.264 encoder answers in Annex B already; an encoder that
/// answers in length-prefixed units instead is turned here, so both backends send the same
/// shape. A stream that is Annex B already is not touched, and a length the reading cannot
/// finish leaves the rest out rather than running past the bytes there are.
fn annex_b(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len() + 12);
    let mut at = 0usize;
    while at + 4 <= bytes.len() {
        let length = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        at += 4;
        if length == 0 || at + length > bytes.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&bytes[at..at + length]);
        at += length;
    }
    out
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;
    use windows::Win32::Media::MediaFoundation::eAVEncH264VProfile_Main;

    #[test]
    fn a_clean_probe_means_hardware_and_any_failure_means_the_download() {
        assert_eq!(choose(&Ok(())), Choice::MediaFoundation);
        assert_eq!(choose(&Err("This machine lists no hardware H.264 encoder.".to_owned())), Choice::OpenH264);
        assert_eq!(choose(&Err(String::new())), Choice::OpenH264);
    }

    /// A share's own 4×4 picture, packed the way the encoder is promised its stride.
    #[test]
    fn nv12_is_the_y_plane_and_the_two_chroma_planes_interleaved() {
        let rgba = vec![255u8; 4 * 4 * 4];
        let planes = crate::picture::i420(&rgba, 4, 4, 4, 4, false);
        let stride = 16;
        let mut out = vec![7u8; 4 * 16 * 3 / 2];
        nv12(&planes, stride, &mut out);
        // Four Y rows at the stride, the padding between and after them untouched.
        for row in 0..4 {
            let at = row * stride;
            assert_eq!(out[at..at + 4], planes.y[row * 4..(row + 1) * 4], "row {row}");
            assert!(out[at + 4..at + stride].iter().all(|&byte| byte == 7), "padding of row {row} was written");
        }
        // Chroma under the luma, one U and one V per pair of pixels.
        for row in 0..2 {
            let at = 4 * stride + row * stride;
            for col in 0..2 {
                assert_eq!(out[at + col * 2], planes.u[row * 2 + col], "u at {row},{col}");
                assert_eq!(out[at + col * 2 + 1], planes.v[row * 2 + col], "v at {row},{col}");
            }
        }
    }

    /// A short output writes what it can and stops, rather than running past the buffer.
    #[test]
    fn nv12_of_a_short_buffer_writes_nothing_past_it() {
        let rgba = vec![128u8; 4 * 4 * 4];
        let planes = crate::picture::i420(&rgba, 4, 4, 4, 4, false);
        let mut out = vec![0u8; 4]; // holds one Y row, no chroma
        nv12(&planes, 4, &mut out);
        assert_eq!(out, planes.y[..4]);
    }

    #[test]
    fn annex_b_already_annex_b_is_untouched() {
        let stream = [0u8, 0, 0, 1, 0x65, 1, 2, 3, 0, 0, 0, 1, 0x41, 4, 5];
        assert_eq!(annex_b(&stream), stream.to_vec());
        let three = [0u8, 0, 1, 0x65, 1, 2, 3];
        assert_eq!(annex_b(&three), three.to_vec());
    }

    /// The other shape an H.264 byte stream takes: four byte lengths, one per unit.
    #[test]
    fn length_prefixed_units_come_out_behind_start_codes() {
        let stream = [0u8, 0, 0, 4, 0x65, 1, 2, 3, 0, 0, 0, 2, 0x41, 4];
        assert_eq!(annex_b(&stream), vec![0, 0, 0, 1, 0x65, 1, 2, 3, 0, 0, 0, 1, 0x41, 4]);
    }

    #[test]
    fn a_truncated_or_empty_stream_never_runs_past_itself() {
        assert!(annex_b(&[]).is_empty());
        // The length says more than there is: nothing comes out, nothing runs past.
        assert!(annex_b(&[0, 0, 0, 99, 0x65]).is_empty());
        assert_eq!(annex_b(&[0, 0, 0, 2, 0x41, 4, 9, 9]), vec![0, 0, 0, 1, 0x41, 4]);
    }

    #[test]
    fn sizes_and_rates_are_packed_width_over_height_and_thousandths() {
        assert_eq!(size(2560, 1440), (2560u64 << 32) | 1440);
        assert_eq!(rate(60.), (60_000u64 << 32) | 1000);
        assert_eq!(rate(29.97), (29_970u64 << 32) | 1000);
        assert_eq!(rate(0.), 1u64 << 32 | 1000);
    }

    /// A transform with no machine behind it: it answers as its fields say and keeps what
    /// it is handed, so the open, the encode and the keyframe flag are held against the
    /// same words the hardware answers, on any machine.
    #[derive(Default)]
    struct FakeMft {
        /// What `set_output_type` answers instead of setting a type.
        output_refusal: Option<String>,
        /// What `set_input_type` answers instead of setting a type.
        input_refusal: Option<String>,
        /// The profile the kept type names: `None` is a type that names none.
        negotiated_profile: Option<u32>,
        /// Events waiting to be read.
        events: VecDeque<Event>,
        /// Frames waiting to be taken.
        outputs: VecDeque<Taken>,
        /// What comes out after a frame goes in, for the frame-loop tests.
        after_give: Option<Taken>,
        /// The frames handed in, as (width, height).
        given: Vec<(usize, usize)>,
        /// The bitrates the tunes asked for.
        tuned: Vec<u32>,
        /// The bitrate the codec properties settled at open.
        configured: Option<u32>,
        keys_forced: u32,
        unlocked: bool,
        output_set: bool,
        input_set: bool,
        began: bool,
        ended: bool,
    }

    impl Mft for FakeMft {
        fn unlock(&mut self) -> Result<(), String> {
            self.unlocked = true;
            Ok(())
        }

        fn set_output_type(&mut self, _width: usize, _height: usize, _bps: u32, _fps: f32, _screen: bool) -> Result<Option<u32>, String> {
            if let Some(why) = self.output_refusal.clone() {
                return Err(why);
            }
            self.output_set = true;
            Ok(self.negotiated_profile)
        }

        fn set_input_type(&mut self, _width: usize, _height: usize, _stride: usize, _frame: usize, _fps: f32) -> Result<(), String> {
            if let Some(why) = self.input_refusal.clone() {
                return Err(why);
            }
            self.input_set = true;
            Ok(())
        }

        fn configure(&mut self, bps: u32) {
            self.configured = Some(bps);
        }

        fn begin(&mut self) -> Result<(), String> {
            self.began = true;
            Ok(())
        }

        fn event(&mut self) -> Result<Option<Event>, String> {
            Ok(self.events.pop_front())
        }

        fn give(&mut self, planes: &Planes, _stride: usize, _frame: usize, _at_100ns: i64, _lasting_100ns: i64) -> Result<(), String> {
            self.given.push((planes.width, planes.height));
            // The input slot reopens behind the frame taken in, the way the machine's
            // encoders answer.
            self.events.push_back(Event::NeedInput);
            if let Some(taken) = self.after_give.clone() {
                self.events.push_back(Event::HaveOutput);
                self.outputs.push_back(taken);
            }
            Ok(())
        }

        fn take(&mut self) -> Result<Option<Taken>, String> {
            Ok(self.outputs.pop_front())
        }

        fn tune(&mut self, bps: u32) -> bool {
            self.tuned.push(bps);
            true
        }

        fn force_key(&mut self) {
            self.keys_forced += 1;
        }

        fn end(&mut self) {
            self.ended = true;
        }
    }

    /// The fake the test keeps a handle into: every ask of the encoder lands in the one
    /// fake both sides see.
    #[derive(Clone)]
    struct SharedFake(Rc<RefCell<FakeMft>>);

    impl SharedFake {
        fn new(fake: FakeMft) -> SharedFake {
            SharedFake(Rc::new(RefCell::new(fake)))
        }

        fn with<R>(&self, read: impl FnOnce(&FakeMft) -> R) -> R {
            read(&self.0.borrow())
        }
    }

    impl Mft for SharedFake {
        fn unlock(&mut self) -> Result<(), String> {
            self.0.borrow_mut().unlock()
        }

        fn set_output_type(&mut self, width: usize, height: usize, bps: u32, fps: f32, screen: bool) -> Result<Option<u32>, String> {
            self.0.borrow_mut().set_output_type(width, height, bps, fps, screen)
        }

        fn set_input_type(&mut self, width: usize, height: usize, stride: usize, frame: usize, fps: f32) -> Result<(), String> {
            self.0.borrow_mut().set_input_type(width, height, stride, frame, fps)
        }

        fn configure(&mut self, bps: u32) {
            self.0.borrow_mut().configure(bps)
        }

        fn begin(&mut self) -> Result<(), String> {
            self.0.borrow_mut().begin()
        }

        fn event(&mut self) -> Result<Option<Event>, String> {
            self.0.borrow_mut().event()
        }

        fn give(&mut self, planes: &Planes, stride: usize, frame: usize, at_100ns: i64, lasting_100ns: i64) -> Result<(), String> {
            self.0.borrow_mut().give(planes, stride, frame, at_100ns, lasting_100ns)
        }

        fn take(&mut self) -> Result<Option<Taken>, String> {
            self.0.borrow_mut().take()
        }

        fn tune(&mut self, bps: u32) -> bool {
            self.0.borrow_mut().tune(bps)
        }

        fn force_key(&mut self) {
            self.0.borrow_mut().force_key()
        }

        fn end(&mut self) {
            self.0.borrow_mut().end()
        }
    }

    // SAFETY: a test's fake lives and dies on the one thread the test runs on, and the
    // encoder is opened, driven and dropped on that same thread.
    unsafe impl Send for SharedFake {}

    /// One fake transform, handed over when the open asks for it.
    struct FakeSource(SharedFake);

    impl Source for FakeSource {
        fn activate(&self) -> Result<Box<dyn Mft>, String> {
            Ok(Box::new(self.0.clone()))
        }
    }

    /// A 4×4 frame the way the share's picture arrives.
    fn planes() -> crate::picture::Planes {
        let rgba = vec![255u8; 4 * 4 * 4];
        crate::picture::i420(&rgba, 4, 4, 4, 4, false)
    }

    /// An open against a fake that keeps the constrained baseline, with its first input
    /// slot already open.
    fn opened(shared: SharedFake) -> Encoder {
        Encoder::open_with(&FakeSource(shared), Platform::unstarted(), 4, 4, 2_500_000, 30., true).expect("the fake encoder opens")
    }

    fn baseline_fake() -> FakeMft {
        let mut fake = FakeMft::default();
        fake.negotiated_profile = Some(BASELINE);
        fake.events.push_back(Event::NeedInput);
        fake
    }

    /// An MFT that refuses the H.264 output type fails the Media Foundation encoder in its
    /// own words, and `choose` sends every such refusal to OpenH264 — the fallback a
    /// failed probe already takes. Nothing is retried without the profile.
    #[test]
    fn an_encoder_that_refuses_the_output_type_is_failed_in_its_own_words() {
        let mut fake = FakeMft::default();
        fake.output_refusal = Some("would not take an H.264 output".to_owned());
        let shared = SharedFake::new(fake);
        let Err(why) = Encoder::open_with(&FakeSource(shared.clone()), Platform::unstarted(), 4, 4, 2_500_000, 30., true) else {
            panic!("a refused output type is a refused encoder");
        };
        assert!(why.contains("H.264 output"), "{why}");
        assert!(!shared.with(|fake| fake.input_set), "the NV12 input ask never came after the refusal");
        // And the refusal is the fallback's cue, the same as a failed probe.
        assert_eq!(choose(&Err(why)), Choice::OpenH264);
    }

    /// A driver that keeps Main or High, or names no profile at all, would quietly change
    /// what goes on the wire; both are refused in words that say why, and the baselines
    /// the SFU was promised are the ones that open.
    #[test]
    fn a_profile_other_than_the_baseline_promised_is_refused() {
        let mut high = FakeMft::default();
        high.negotiated_profile = Some(eAVEncH264VProfile_Main.0 as u32);
        let Err(why) = Encoder::open_with(&FakeSource(SharedFake::new(high)), Platform::unstarted(), 4, 4, 2_500_000, 30., true) else {
            panic!("Main is not the baseline the SFU takes");
        };
        assert!(why.contains("not the baseline"), "{why}");
        assert_eq!(choose(&Err(why)), Choice::OpenH264);

        let mut silent = FakeMft::default();
        silent.negotiated_profile = None;
        let Err(why) = Encoder::open_with(&FakeSource(SharedFake::new(silent)), Platform::unstarted(), 4, 4, 2_500_000, 30., true) else {
            panic!("a kept type that names no profile cannot be held to the promise");
        };
        assert!(why.contains("profile"), "{why}");

        // Constrained baseline and plain baseline are the promise kept.
        for kept in [CONSTRAINED_BASELINE, BASELINE] {
            let mut fake = FakeMft::default();
            fake.negotiated_profile = Some(kept);
            fake.events.push_back(Event::NeedInput);
            drop(opened(SharedFake::new(fake)));
        }
    }

    /// `MFSampleExtension_CleanPoint` says keyframe only by its value: a delta frame that
    /// carries the extension as zero, and a sample that does not carry it at all, are not
    /// keyframes, whatever a mere "the attribute is there" would have said.
    #[test]
    fn a_frame_is_a_keyframe_only_when_the_sample_says_it_is_a_clean_point() {
        assert!(!clean_point_is_key(None));
        assert!(!clean_point_is_key(Some(0)));
        assert!(clean_point_is_key(Some(1)));
        for (clean, key) in [(Some(1u32), true), (Some(0u32), false), (None, false)] {
            let mut fake = baseline_fake();
            fake.after_give = Some(Taken { bytes: vec![0, 0, 0, 1, 0x65], clean });
            let mut encoder = opened(SharedFake::new(fake));
            let encoded = encoder.encode(&planes(), &Ask::default()).expect("the fake encoder answers");
            assert_eq!(encoded.key, key, "clean point {clean:?}");
            assert!(!encoded.bytes.is_empty(), "the frame's bits came out");
        }
    }

    /// An open encoder takes frames: the frames go in at the size it was opened for, the
    /// keyframe ask reaches the codec, the codec properties settle at open, and a tune is
    /// only sent when the ask moves.
    #[test]
    fn an_open_encoder_takes_frames_and_holds_the_asks() {
        let mut fake = baseline_fake();
        fake.after_give = Some(Taken { bytes: vec![0, 0, 0, 1, 0x65], clean: Some(1) });
        let shared = SharedFake::new(fake);
        let mut encoder = opened(shared.clone());

        let asked = Ask { key: true, ..Ask::default() };
        let out = encoder.encode(&planes(), &asked).expect("the first frame answers");
        assert!(out.key, "the first output is a clean point");
        shared.with(|fake| {
            assert!(fake.unlocked && fake.output_set && fake.input_set && fake.began, "the open ran whole");
            assert_eq!(fake.configured, Some(2_500_000), "the codec properties settled at open");
            assert_eq!(fake.given, vec![(4, 4)], "the frame went in");
            assert_eq!(fake.keys_forced, 1, "the keyframe ask reached the codec");
            assert!(fake.tuned.is_empty(), "the ask did not move, so no tune went out");
        });

        // The same rate again: no tune. A moved rate: one tune, and the encoder keeps it.
        encoder.encode(&planes(), &Ask::default()).expect("the second frame answers");
        let moved = Ask { bps: 1_000_000, ..Ask::default() };
        encoder.encode(&planes(), &moved).expect("the third frame answers");
        shared.with(|fake| {
            assert_eq!(fake.tuned, vec![1_000_000], "only the moved ask is tuned");
            assert_eq!(fake.keys_forced, 1, "no keyframe ask without one");
            assert_eq!(fake.given.len(), 3, "every frame with an open slot went in");
        });
    }

    /// A close is a flush and an end of streaming, said to the transform before it goes.
    #[test]
    fn a_closed_encoder_ends_its_stream() {
        let shared = SharedFake::new(baseline_fake());
        drop(opened(shared.clone()));
        shared.with(|fake| assert!(fake.ended, "the close reached the transform"));
    }

    /// The probe answers on a thread of its own, so the machine's words are taken on a
    /// fresh apartment and the asking thread's is never asked. Here it is held against
    /// running whole: it answers, once, and its words are its own either way.
    #[test]
    fn the_probe_answers_on_a_thread_of_its_own() {
        let answer = probe_thread().expect("the probe's thread starts");
        // Whatever this machine answers, the probe ran whole: COM came up and went, and
        // Media Foundation with it. A machine with no hardware MFT is answered in words.
        if let Err(why) = answer {
            assert!(!why.is_empty(), "a refusal says why");
        }
    }
}
