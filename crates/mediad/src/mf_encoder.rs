//! The Windows Media Foundation H.264 encoder, the machine's own hardware ahead of the
//! OpenH264 download (`picture/desktop`), which stays as the fallback.
//!
//! A hardware encoder is an asynchronous Media Foundation transform (MFT): it is found with
//! `MFTEnumEx` over the registered hardware encoders, woken by `MF_TRANSFORM_ASYNC_UNLOCK`,
//! fed NV12 frames through `ProcessInput`, and drained through its event queue
//! (`METransformNeedInput`, `METransformHaveOutput`) with `ProcessOutput`. The rate is CBR
//! at the bitrate the track asked for (`MF_MT_AVG_BITRATE` and
//! `CODECAPI_AVEncCommonRateControlMode`), a keyframe is asked for with
//! `CODECAPI_AVEncVideoForceKeyFrame`, and the output is normalized to the Annex B byte
//! stream the SFU and the OpenH264 decoder take. A frame whose bits are not out yet comes
//! back as an empty [`Encoded`], which the share already treats as a skip; they leave on a
//! later frame's pump.
//!
//! Everything here runs where the encoder was opened: the share's or camera's capture
//! thread, COM in the Multi Threaded Apartment, Media Foundation started without its
//! sockets. `Outgoing` only ever moves between threads before an encoder exists, and MTA
//! objects are callable from every thread of the process, so the `Send` on [`Encoder`] is
//! honest. What the encoder keeps between frames is its bitrate, its frame rate, and
//! whether it has an input slot to fill; a change of frame size is not a tune but a new
//! encoder (`coded` in `desktop`), as with OpenH264.

use std::ptr::null_mut;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, VARIANT_BOOL};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonLowLatency, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    IMFActivate, IMFCodecAPI, IMFMediaEventGenerator, IMFTransform, MFVideoInterlace_Progressive,
    MF_E_NO_EVENTS_AVAILABLE, MF_EVENT_FLAG_NO_WAIT, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE,
    MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, METransformHaveOutput, METransformNeedInput,
    MFTEnumEx, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFStartup, MFSTARTUP_NOSOCKET,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFSampleExtension_CleanPoint, eAVEncCommonRateControlMode_CBR,
    eAVEncH264VLevel3_1, eAVEncH264VLevel5_1, eAVEncH264VProfile_ConstrainedBase,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VT_BOOL, VT_UI4, VARIANT, VariantInit};

use crate::picture::{Ask, Encoded, Planes};

/// How long one frame waits for the machine's encoder to answer. Hardware encoders in low
/// latency answer inside a frame's time; when one does not, the frame goes out empty and
/// its bits leave on the next pump, the way a skip already does.
const ANSWER_WAIT: Duration = Duration::from_millis(120);
/// How often the event queue is looked at while waiting for that answer.
const POLL: Duration = Duration::from_millis(1);
/// The NV12 stride promised to the encoder, the width rounded up as the hardware wants it.
const STRIDE_ALIGN: usize = 16;

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
/// system again before it opens.
pub fn available() -> Result<(), String> {
    static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
    AVAILABLE.get_or_init(probe).clone()
}

/// One look at what the machine lists, without opening anything. Fails in its own words: a
/// machine with no hardware MFT shares through OpenH264, and so does one whose COM or Media
/// Foundation would not start.
fn probe() -> Result<(), String> {
    platform()?;
    if enumerate() == 0 {
        return Err("This machine lists no hardware H.264 encoder.".to_owned());
    }
    Ok(())
}

/// How many hardware H.264 encoders this machine lists, taking NV12 and answering H.264.
fn enumerate() -> u32 {
    let takes = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let answers = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut list: *mut Option<IMFActivate> = null_mut();
    let mut count = 0u32;
    // SAFETY: both registration types outlive the call, and the list it hands back is freed
    // whole below, after the count is read. Nothing here keeps a handle into it.
    let _ = unsafe {
        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, Some(&takes), Some(&answers), &mut list, &mut count)
    };
    // SAFETY: the list is MFTEnumEx's own allocation, freed whole; no element is read.
    unsafe { CoTaskMemFree(Some(list.cast::<core::ffi::c_void>())) };
    count
}

/// COM in the Multi Threaded Apartment, per thread, and Media Foundation, per process.
/// Both keep count, so an encoder thread that meets another's work answers at once.
fn platform() -> Result<(), String> {
    // SAFETY: the apartment call is the documented per-thread init; its result is checked.
    let started = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if started.is_err() {
        return Err(if started == RPC_E_CHANGED_MODE {
            "This thread already holds a COM apartment the encoder cannot share.".to_owned()
        } else {
            format!("COM would not start for the encoder: {started}")
        });
    }
    // The init is never undone: the capture thread outlives the encoder by a frame at most,
    // and Media Foundation's own teardown races nothing that outlives it here.
    // SAFETY: the version and the no-sockets flag are the constants Media Foundation reads.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|err| format!("Media Foundation would not start: {err}"))
}

/// The machine's hardware H.264 encoder, held as the async MFT, its event queue, and the
/// codec properties that survive a frame.
pub struct Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// `None` when the MFT exposes no codec properties, which leaves a keyframe ask and a
    /// bitrate move without their handle; the picture still encodes.
    codec: Option<IMFCodecAPI>,
    width: usize,
    height: usize,
    /// The stride promised in the input type: the width rounded up as the hardware wants.
    stride: usize,
    /// One NV12 frame at that stride: the Y plane and the two interleaved chroma planes.
    frame: usize,
    /// What the encoder holds now, so a tune is only sent when the ask moved.
    bps: u32,
    fps: f32,
    /// Whether the encoder has an input slot open: a `METransformNeedInput` seen and not
    /// yet fed.
    wants_input: bool,
}

/// SAFETY: the encoder is opened, driven, and dropped on one thread (the share's or the
/// camera's capture thread); `Outgoing` moves only before any encoder exists. The COM
/// apartment is the MTA and Media Foundation objects answer from every thread of the
/// process, so even a move after the fact would stay within what the platform allows.
unsafe impl Send for Encoder {}

impl Encoder {
    /// The first hardware encoder that takes NV12 and answers H.264, set up for CBR at
    /// `bps` and at most `fps`, level 5.1 for a screen and 3.1 for a camera, at the profile
    /// the SFU takes. Its own words when there is no such encoder or one would not take its
    /// setup; the caller falls back to OpenH264 on any of them.
    pub fn open(width: u32, height: u32, bps: u32, fps: f32, screen: bool) -> Result<Encoder, String> {
        platform()?;
        let (width, height) = (width.max(2) as usize, height.max(2) as usize);
        let stride = (width + STRIDE_ALIGN - 1) & !(STRIDE_ALIGN - 1);
        let frame = stride * height * 3 / 2;

        let mut list: *mut Option<IMFActivate> = null_mut();
        let mut count = 0u32;
        let takes = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let answers = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
        // SAFETY: as in `enumerate`, the registration types outlive the call and the list is
        // freed below, after its first entry has been taken out of it.
        unsafe { MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, Some(&takes), Some(&answers), &mut list, &mut count) }
            .map_err(|err| format!("the hardware encoders could not be listed: {err}"))?;
        let first = if count > 0 {
            // SAFETY: `count` says the list holds at least one entry, and reading it moves
            // the interface out of MFTEnumEx's allocation before that allocation is freed.
            Some(unsafe { std::ptr::read(list.add(0)) })
        } else {
            None
        };
        // SAFETY: as above, the list is freed whole.
        unsafe { CoTaskMemFree(Some(list.cast::<core::ffi::c_void>())) };
        let Some(activate) = first.flatten() else {
            return Err("This machine lists no hardware H.264 encoder.".to_owned());
        };
        // SAFETY: the activation object hands out the transform it stands for, once.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .map_err(|err| format!("the hardware encoder could not be started: {err}"))?;

        // SAFETY: from here on every call is on interfaces this open creates or this
        // encoder holds, whose pointers the calls only read.
        unsafe {
            // A hardware encoder answers on its own clock. The unlock tells Media
            // Foundation this client will drive it through its event queue, before any
            // type is set.
            let attributes = transform.GetAttributes().map_err(|err| format!("the hardware encoder told nothing about itself: {err}"))?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(|err| format!("the hardware encoder could not be unlocked: {err}"))?;

            let input = input_type(width, height, stride, frame, fps)?;
            transform.SetInputType(0, &input, 0).map_err(|err| format!("the hardware encoder would not take NV12: {err}"))?;

            match output_type(width, height, bps, fps, screen, true) {
                Ok(output) => {
                    if transform.SetOutputType(0, &output, 0).is_err() {
                        // A profile or level this encoder does not keep is its own to pick:
                        // ask again without them, and say so once.
                        let plain = output_type(width, height, bps, fps, screen, false)?;
                        transform.SetOutputType(0, &plain, 0)
                            .map_err(|err| format!("the hardware encoder would not take an H.264 output: {err}"))?;
                        tracing::debug!("mediad: the hardware encoder chose its own H.264 profile.");
                    }
                }
                Err(why) => return Err(why),
            }

            let codec: Option<IMFCodecAPI> = transform.cast().ok();
            if let Some(codec) = &codec {
                // Both rates are said twice on purpose: once on the output type, which the
                // MFT reads as it opens, and once through the codec properties, which the
                // rate control holds for the frames after. A refusal is logged and lived
                // with: the encoder still answers, at a rate of its own choosing.
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

            let events: IMFMediaEventGenerator = transform.cast().map_err(|err| format!("the hardware encoder has no event queue: {err}"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|err| format!("the hardware encoder would not start streaming: {err}"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|err| format!("the hardware encoder would not start streaming: {err}"))?;

            let mut encoder = Encoder { transform, events, codec, width, height, stride, frame, bps, fps, wants_input: false };
            // The first input slot is usually open before the first frame; wait a moment
            // for it, so the first frame of a share is fed at once and not dropped.
            let until = Instant::now() + ANSWER_WAIT;
            while !encoder.wants_input && Instant::now() < until {
                encoder.pump(&mut Vec::new(), &mut false)?;
                if !encoder.wants_input {
                    std::thread::sleep(POLL);
                }
            }
            Ok(encoder)
        }
    }

    /// One frame in, whatever is out. A frame whose bits are still on their way comes back
    /// empty, like a skip; an encoded frame comes back marked when it is a keyframe, so
    /// the share sends it as one.
    pub fn encode(&mut self, planes: &Planes, ask: &Ask) -> Result<Encoded, String> {
        if planes.width != self.width || planes.height != self.height {
            return Err(format!("The encoder was opened for {}×{} and handed {}×{}.", self.width, self.height, planes.width, planes.height));
        }
        self.tune(ask);
        if ask.key {
            self.request_key();
        }
        // What is already out, before this frame goes in: the machine may be a frame ahead.
        let mut bytes = Vec::new();
        let mut key = false;
        self.pump(&mut bytes, &mut key)?;
        if self.wants_input {
            self.submit(planes, ask)?;
            // A bounded moment for this frame's own bits. When they are not out in time,
            // the empty frame goes and the pump catches them next time.
            let until = Instant::now() + ANSWER_WAIT;
            while bytes.is_empty() && Instant::now() < until {
                self.pump(&mut bytes, &mut key)?;
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

    /// The rate asked for, if it moved. The frame rate is set at open and is not moved
    /// again; a moving ask for it is a no-op here.
    fn tune(&mut self, ask: &Ask) {
        if ask.bps == self.bps || ask.bps == 0 {
            return;
        }
        let Some(codec) = &self.codec else {
            return;
        };
        // SAFETY: the property is read through the codec handle this encoder holds, and
        // the variant outlives the call.
        match unsafe { codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &counted(ask.bps)) } {
            Ok(()) => self.bps = ask.bps,
            Err(err) => tracing::warn!("mediad: the hardware encoder refused a bitrate of {}: {err}", ask.bps),
        }
    }

    /// A keyframe for the next frame in. The property carries no value; setting it is the
    /// ask. An encoder without codec properties cannot be asked, and keeps its own
    /// keyframe pace.
    fn request_key(&mut self) {
        let Some(codec) = &self.codec else {
            return;
        };
        // SAFETY: as in `tune`, with a property whose value is ignored.
        if let Err(err) = unsafe { codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &counted(0)) } {
            tracing::debug!("mediad: the hardware encoder refused a keyframe: {err}");
        }
    }

    /// This frame as a sample, its planes packed NV12 at the promised stride.
    fn submit(&mut self, planes: &Planes, ask: &Ask) -> Result<(), String> {
        // SAFETY: the sample is created here, filled here, and handed to the transform,
        // which owns its reference from `ProcessInput` on; the buffer pointer is used
        // within its lock below and gone before the lock is let go.
        unsafe {
            let buffer = MFCreateMemoryBuffer(self.frame as u32).map_err(|err| format!("the frame could not be given room: {err}"))?;
            let mut data: *mut u8 = null_mut();
            buffer.Lock(&mut data, None, None).map_err(|err| format!("the frame could not be written: {err}"))?;
            nv12(planes, self.stride, std::slice::from_raw_parts_mut(data, self.frame));
            let _ = buffer.SetCurrentLength(self.frame as u32);
            buffer.Unlock().map_err(|err| format!("the frame could not be handed over: {err}"))?;

            let sample = MFCreateSample().map_err(|err| format!("the frame could not be wrapped: {err}"))?;
            sample.AddBuffer(&buffer).map_err(|err| format!("the frame could not be wrapped: {err}"))?;
            // The ask's clock is milliseconds; Media Foundation's is 100 nanoseconds.
            sample.SetSampleTime((ask.at.as_millis() as i64).saturating_mul(10_000)).map_err(|err| format!("the frame's time was refused: {err}"))?;
            let lasting = (10_000_000.0 / self.fps.max(1.0)) as i64;
            sample.SetSampleDuration(lasting.max(1)).map_err(|err| format!("the frame's length was refused: {err}"))?;
            self.transform.ProcessInput(0, &sample, 0).map_err(|err| format!("the hardware encoder would not take the frame: {err}"))?;
            self.wants_input = false;
            Ok(())
        }
    }

    /// Everything the event queue holds now, without waiting: frames out, and the open
    /// input slot remembered for the next frame in.
    fn pump(&mut self, bytes: &mut Vec<u8>, key: &mut bool) -> Result<(), String> {
        loop {
            // SAFETY: the event generator belongs to this encoder and is not held
            // elsewhere; each event is this call's own, read once and dropped.
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(err) => return Err(format!("the hardware encoder's events failed: {err}")),
            };
            let kind = unsafe { event.GetType() }.map_err(|err| format!("the hardware encoder's events could not be read: {err}"))?;
            if kind == METransformHaveOutput.0 as u32 {
                self.take_output(bytes, key)?;
            } else if kind == METransformNeedInput.0 as u32 {
                self.wants_input = true;
            }
            // METransformDrainComplete and METransformInputStreamStateChanged need nothing
            // from a share: nothing is ever drained, and the input state is read back
            // through `wants_input` alone.
        }
    }

    /// One encoded frame out of the transform, its bytes appended and its keyframe-ness
    /// remembered.
    fn take_output(&mut self, bytes: &mut Vec<u8>, key: &mut bool) -> Result<(), String> {
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
                return Ok(());
            };
            if sample.GetUINT32(&MFSampleExtension_CleanPoint).is_ok() {
                *key = true;
            }
            // The sample's buffers are made one here, whatever count the encoder used.
            let contiguous = sample.ConvertToContiguousBuffer().map_err(|err| format!("the encoded frame could not be read: {err}"))?;
            let mut data: *mut u8 = null_mut();
            let mut length = 0u32;
            contiguous.Lock(&mut data, None, Some(&mut length)).map_err(|err| format!("the encoded frame could not be read: {err}"))?;
            bytes.extend_from_slice(&annex_b(std::slice::from_raw_parts(data, length as usize)));
            contiguous.Unlock().map_err(|err| format!("the encoded frame could not be let go: {err}"))?;
            Ok(())
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the transform is still owned here; both messages are its own.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// The input type the encoder is fed: NV12 at the promised stride, whole frames, at the
/// size and rate the track asked for.
fn input_type(width: usize, height: usize, stride: usize, frame: usize, fps: f32) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType, String> {
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
/// profile the SFU takes when `with_profile` says so. An encoder that keeps none of them
/// is asked again without.
fn output_type(width: usize, height: usize, bps: u32, fps: f32, screen: bool, with_profile: bool) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType, String> {
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
        if with_profile {
            // Constrained baseline is what the SFU takes, and what OpenH264 sends; a level
            // holds the size and rate, 5.1 for a 1440p share at 60.
            kind.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_ConstrainedBase.0 as u32).map_err(|err| format!("the output type could not be built: {err}"))?;
            kind.SetUINT32(&MF_MT_MPEG2_LEVEL, level.0 as u32).map_err(|err| format!("the output type could not be built: {err}"))?;
        }
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
    // SAFETY: a VARIANT is initialized here and read by the property call it is handed to.
    unsafe {
        let mut value = VariantInit();
        value.Anonymous.Anonymous.vt = VT_UI4;
        value.Anonymous.Anonymous.Anonymous.ulVal = count;
        value
    }
}

/// A `VARIANT` holding TRUE, for the switches.
fn yes() -> VARIANT {
    // SAFETY: as in `counted`, with a boolean.
    unsafe {
        let mut value = VariantInit();
        value.Anonymous.Anonymous.vt = VT_BOOL;
        value.Anonymous.Anonymous.Anonymous.boolVal = VARIANT_BOOL(-1);
        value
    }
}

/// `planes` as NV12 into `out`, Y rows at `stride` and the two chroma planes interleaved
/// under them. The padding rows and bytes stay what the buffer was made with, and a buffer
/// too short takes what fits and nothing past it.
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
    use super::*;

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
}
