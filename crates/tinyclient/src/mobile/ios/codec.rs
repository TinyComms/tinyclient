//! H.264 through VideoToolbox: `VTCompressionSession` (constrained baseline, real time, no
//! frame reordering) and `VTDecompressionSession`, both used synchronously. VideoToolbox speaks
//! AVCC (length-prefixed NAL units, parameter sets in the format description); mediad speaks
//! Annex B (start codes, SPS and PPS in band), so both directions convert here.

use super::Shared;
use mediad::platform::{EncoderConfig, H264Decoder, H264Encoder, VideoFrame};
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{
    kCMTimeInvalid, kCMVideoCodecType_H264, CMBlockBuffer, CMFormatDescription, CMSampleBuffer,
    CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVImageBuffer, CVPixelBuffer, CVPixelBufferCreate,
};
use objc2_foundation::{NSCopying, NSDictionary, NSNumber, NSString};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTDecompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel, VTCompressionSession, VTDecodeFrameFlags, VTDecodeInfoFlags,
    VTDecompressionOutputCallbackRecord, VTDecompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
};
use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::Mutex;

const START: [u8; 4] = [0, 0, 0, 1];

/// A dictionary of CF string keys to numbers, as the CF type VideoToolbox takes.
fn dictionary(pairs: &[(&CFString, CFValue)]) -> objc2::rc::Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<&NSString> = pairs
        .iter()
        // SAFETY: CFString and NSString are toll-free bridged.
        .map(|(key, _)| unsafe { &*(*key as *const CFString as *const NSString) })
        .collect();
    let values: Vec<objc2::rc::Retained<AnyObject>> = pairs.iter().map(|(_, value)| value.object()).collect();
    let refs: Vec<&AnyObject> = values.iter().map(|value| &**value).collect();
    NSDictionary::from_slices(&keys, &refs)
}

/// A value for [`dictionary`] or [`set`].
#[derive(Clone, Copy)]
enum CFValue {
    Bool(bool),
    Int(i64),
    Str(&'static CFString),
}

impl CFValue {
    fn object(self) -> objc2::rc::Retained<AnyObject> {
        match self {
            CFValue::Bool(value) => objc2::rc::Retained::into_super(objc2::rc::Retained::into_super(NSNumber::new_bool(value))).into(),
            CFValue::Int(value) => objc2::rc::Retained::into_super(objc2::rc::Retained::into_super(NSNumber::new_i64(value))).into(),
            // SAFETY: CFString and NSString are toll-free bridged; retained for the copy.
            CFValue::Str(value) => unsafe {
                let string: &NSString = &*(value as *const CFString as *const NSString);
                objc2::rc::Retained::into_super(string.copy()).into()
            },
        }
    }
}

/// The CF view of an NSDictionary.
fn cf(dictionary: &NSDictionary<NSString, AnyObject>) -> &CFDictionary {
    // SAFETY: NSDictionary and CFDictionary are toll-free bridged.
    unsafe { &*(dictionary as *const NSDictionary<NSString, AnyObject> as *const CFDictionary) }
}

/// Sets one property on a compression or decompression session.
fn set(session: &CFType, key: &CFString, value: CFValue) {
    let object = value.object();
    // SAFETY: every NSObject here is a toll-free bridged CF object.
    let value: &CFType = unsafe { &*(&*object as *const AnyObject as *const CFType) };
    // SAFETY: a valid session, key and value.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    if status != 0 {
        log::debug!("tinyclient: VideoToolbox property {key} not taken ({status})");
    }
}

/// Splits Annex B into NAL units (without start codes).
fn nal_units(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut index = 0;
    let mut start = None;
    while index + 3 <= annex_b.len() {
        let three = annex_b[index] == 0 && annex_b[index + 1] == 0 && annex_b[index + 2] == 1;
        if three {
            if let Some(from) = start {
                let mut end = index;
                while end > from && annex_b[end - 1] == 0 {
                    end -= 1;
                }
                units.push(&annex_b[from..end]);
            }
            index += 3;
            start = Some(index);
        } else {
            index += 1;
        }
    }
    if let Some(from) = start {
        if from < annex_b.len() {
            units.push(&annex_b[from..]);
        }
    }
    units.into_iter().filter(|unit| !unit.is_empty()).collect()
}

/// AVCC (4-byte big-endian lengths) to Annex B.
fn avcc_to_annex_b(avcc: &[u8], out: &mut Vec<u8>) -> bool {
    let mut idr = false;
    let mut index = 0;
    while index + 4 <= avcc.len() {
        let length = u32::from_be_bytes([avcc[index], avcc[index + 1], avcc[index + 2], avcc[index + 3]]) as usize;
        index += 4;
        if length == 0 || index + length > avcc.len() {
            break;
        }
        idr |= avcc[index] & 0x1f == 5;
        out.extend_from_slice(&START);
        out.extend_from_slice(&avcc[index..index + length]);
        index += length;
    }
    idr
}

/// What the encoder's callback wrote for the frame being completed.
struct Encoded {
    bytes: Vec<u8>,
    failed: Option<i32>,
}

/// One VideoToolbox H.264 encoder.
pub(super) struct Encoder {
    session: Shared<CFRetained<VTCompressionSession>>,
    out: Box<Mutex<Encoded>>,
    config: EncoderConfig,
    frames: i64,
}

unsafe extern "C-unwind" fn encoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the encoder's boxed `Mutex<Encoded>`, alive while the session is.
    let out = unsafe { &*(refcon as *const Mutex<Encoded>) };
    let Ok(mut out) = out.lock() else {
        return;
    };
    if status != 0 {
        out.failed = Some(status);
        return;
    }
    // SAFETY: VideoToolbox's sample buffer, valid for the call, or null for a dropped frame.
    let Some(sample) = (unsafe { sample.as_ref() }) else {
        return;
    };
    // SAFETY: reading the sample's own buffers through CoreMedia.
    unsafe {
        let Some(data) = sample.data_buffer() else {
            return;
        };
        let length = data.data_length();
        let mut avcc = vec![0u8; length];
        if length == 0 || data.copy_data_bytes(0, length, NonNull::new_unchecked(avcc.as_mut_ptr().cast())) != 0 {
            return;
        }
        let mut frame = Vec::with_capacity(length + 64);
        let idr = avcc_to_annex_b(&avcc, &mut frame);
        if idr {
            // SPS and PPS go before every IDR, in band.
            if let Some(format) = sample.format_description() {
                let mut sets = Vec::new();
                let mut count = 0usize;
                let mut index = 0usize;
                loop {
                    let mut pointer: *const u8 = std::ptr::null();
                    let mut size = 0usize;
                    let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        &format,
                        index,
                        &mut pointer,
                        &mut size,
                        &mut count,
                        null_mut(),
                    );
                    if status != 0 || pointer.is_null() {
                        break;
                    }
                    sets.extend_from_slice(&START);
                    sets.extend_from_slice(std::slice::from_raw_parts(pointer, size));
                    index += 1;
                    if index >= count {
                        break;
                    }
                }
                sets.extend_from_slice(&frame);
                frame = sets;
            }
        }
        out.bytes.extend_from_slice(&frame);
    }
}

impl Encoder {
    pub(super) fn new(config: EncoderConfig) -> Result<Self, String> {
        let out = Box::new(Mutex::new(Encoded { bytes: Vec::new(), failed: None }));
        let attributes = dictionary(&[
            // SAFETY (all three): Core Video's constant keys.
            (unsafe { kCVPixelBufferPixelFormatTypeKey }, CFValue::Int(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i64)),
            (unsafe { kCVPixelBufferWidthKey }, CFValue::Int(config.width as i64)),
            (unsafe { kCVPixelBufferHeightKey }, CFValue::Int(config.height as i64)),
        ]);
        let mut session: *mut VTCompressionSession = null_mut();
        // SAFETY: VideoToolbox's documented creation; the refcon outlives the session (the box
        // is dropped after the session is invalidated, in `Drop`).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                config.width as i32,
                config.height as i32,
                kCMVideoCodecType_H264,
                None,
                Some(cf(&attributes)),
                None,
                Some(encoded),
                &*out as *const Mutex<Encoded> as *mut c_void,
                NonNull::from(&mut session),
            )
        };
        let session = NonNull::new(session)
            .filter(|_| status == 0)
            // SAFETY: a session created with +1 retain.
            .map(|session| unsafe { CFRetained::from_raw(session) })
            .ok_or_else(|| format!("The video encoder could not start ({status})."))?;
        let as_session: &CFType = &session;
        // SAFETY: VideoToolbox's constant keys.
        unsafe {
            set(as_session, kVTCompressionPropertyKey_RealTime, CFValue::Bool(true));
            set(as_session, kVTCompressionPropertyKey_ProfileLevel, CFValue::Str(kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel));
            set(as_session, kVTCompressionPropertyKey_AllowFrameReordering, CFValue::Bool(false));
            set(as_session, kVTCompressionPropertyKey_AverageBitRate, CFValue::Int(config.bitrate_bps as i64));
            set(as_session, kVTCompressionPropertyKey_ExpectedFrameRate, CFValue::Int(config.fps.max(1) as i64));
            set(as_session, kVTCompressionPropertyKey_MaxKeyFrameInterval, CFValue::Int(config.fps.max(1) as i64 * 4));
            session.prepare_to_encode_frames();
        }
        log::info!("tinyclient: H.264 encoder {}x{} at {} bit/s", config.width, config.height, config.bitrate_bps);
        Ok(Self { session: Shared(session), out, config, frames: 0 })
    }
}

impl H264Encoder for Encoder {
    fn encode(&mut self, frame: &VideoFrame, keyframe: bool) -> Result<Vec<u8>, String> {
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err("The encoder was given a frame of the wrong size.".to_owned());
        }
        let session = &self.session.0;
        // SAFETY: a pixel buffer of the session's own size and format, filled and handed to the
        // session, then completed synchronously so the callback has run when this returns.
        unsafe {
            let mut buffer: *mut CVPixelBuffer = null_mut();
            let status = CVPixelBufferCreate(
                None,
                frame.width as usize,
                frame.height as usize,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut buffer),
            );
            let buffer = NonNull::new(buffer)
                .filter(|_| status == 0)
                .map(|buffer| CFRetained::<CVPixelBuffer>::from_raw(buffer))
                .ok_or_else(|| format!("The encoder's picture buffer could not be made ({status})."))?;
            super::pixels::fill_nv12(&buffer, frame)?;
            let properties = keyframe.then(|| dictionary(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, CFValue::Bool(true))]));
            let fps = self.config.fps.max(1) as i32;
            let time = CMTime::new(self.frames, fps);
            self.frames += 1;
            let image: &CVImageBuffer = &buffer;
            let status = session.encode_frame(
                image,
                time,
                kCMTimeInvalid,
                properties.as_deref().map(cf),
                null_mut(),
                null_mut(),
            );
            if status != 0 {
                return Err(format!("The video encoder refused a frame ({status})."));
            }
            session.complete_frames(kCMTimeInvalid);
        }
        let mut out = self.out.lock().map_err(|_| "The video encoder failed.".to_owned())?;
        if let Some(status) = out.failed.take() {
            return Err(format!("The video encoder failed ({status})."));
        }
        Ok(std::mem::take(&mut out.bytes))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the session is done with; its callback cannot run after this.
        unsafe { self.session.0.invalidate() };
    }
}

/// One VideoToolbox H.264 decoder, made when the first SPS and PPS arrive.
pub(super) struct Decoder {
    session: Option<Shared<CFRetained<VTDecompressionSession>>>,
    format: Option<Shared<CFRetained<CMFormatDescription>>>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    out: Box<Mutex<Option<VideoFrame>>>,
    keyframe_seen: bool,
}

unsafe extern "C-unwind" fn decoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _time: CMTime,
    _duration: CMTime,
) {
    if status != 0 {
        return;
    }
    // SAFETY: the refcon is the decoder's boxed slot, alive while the session is; the image is
    // VideoToolbox's, valid for the call.
    unsafe {
        let out = &*(refcon as *const Mutex<Option<VideoFrame>>);
        let Some(image) = image.as_ref() else {
            return;
        };
        let pixels: &CVPixelBuffer = image;
        if let (Some(frame), Ok(mut slot)) = (super::pixels::frame_of(pixels), out.lock()) {
            *slot = Some(frame);
        }
    }
}

impl Decoder {
    pub(super) fn new() -> Self {
        Self {
            session: None,
            format: None,
            sps: Vec::new(),
            pps: Vec::new(),
            out: Box::new(Mutex::new(None)),
            keyframe_seen: false,
        }
    }

    /// Makes (or remakes) the session for the current SPS and PPS.
    fn open(&mut self) -> Result<(), String> {
        self.close();
        let pointers = [NonNull::from(&self.sps[0]), NonNull::from(&self.pps[0])];
        let sizes = [self.sps.len(), self.pps.len()];
        let mut format: *const CMFormatDescription = std::ptr::null();
        // SAFETY: two parameter sets, borrowed for the call.
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                2,
                NonNull::from(&pointers[0]),
                NonNull::from(&sizes[0]),
                4,
                NonNull::from(&mut format),
            )
        };
        let format = NonNull::new(format as *mut CMFormatDescription)
            .filter(|_| status == 0)
            // SAFETY: created with +1 retain.
            .map(|format| unsafe { CFRetained::from_raw(format) })
            .ok_or_else(|| format!("The video's parameters could not be read ({status})."))?;
        let attributes = dictionary(&[(
            // SAFETY: Core Video's constant key.
            unsafe { kCVPixelBufferPixelFormatTypeKey },
            CFValue::Int(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i64),
        )]);
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decoded),
            decompressionOutputRefCon: &*self.out as *const Mutex<Option<VideoFrame>> as *mut c_void,
        };
        let mut session: *mut VTDecompressionSession = null_mut();
        // SAFETY: VideoToolbox's documented creation; the refcon outlives the session.
        let status = unsafe {
            VTDecompressionSession::create(None, &format, None, Some(cf(&attributes)), &record, NonNull::from(&mut session))
        };
        let session = NonNull::new(session)
            .filter(|_| status == 0)
            // SAFETY: created with +1 retain.
            .map(|session| unsafe { CFRetained::from_raw(session) })
            .ok_or_else(|| format!("The video decoder could not start ({status})."))?;
        // SAFETY: VideoToolbox's constant key.
        unsafe { set(&session, kVTDecompressionPropertyKey_RealTime, CFValue::Bool(true)) };
        self.session = Some(Shared(session));
        self.format = Some(Shared(format));
        Ok(())
    }

    fn close(&mut self) {
        if let Some(session) = self.session.take() {
            // SAFETY: done with; the callback cannot run after this.
            unsafe {
                session.0.wait_for_asynchronous_frames();
                session.0.invalidate();
            }
        }
        self.format = None;
    }
}

impl H264Decoder for Decoder {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String> {
        let mut avcc = Vec::with_capacity(annex_b.len() + 16);
        let mut changed = false;
        let mut idr = false;
        for unit in nal_units(annex_b) {
            match unit[0] & 0x1f {
                7 => {
                    changed |= self.sps != unit;
                    self.sps = unit.to_vec();
                }
                8 => {
                    changed |= self.pps != unit;
                    self.pps = unit.to_vec();
                }
                kind @ (1 | 5) => {
                    idr |= kind == 5;
                    avcc.extend_from_slice(&(unit.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(unit);
                }
                _ => {}
            }
        }
        if (changed || self.session.is_none()) && !self.sps.is_empty() && !self.pps.is_empty() {
            self.open()?;
        }
        self.keyframe_seen |= idr;
        let (Some(session), Some(format)) = (&self.session, &self.format) else {
            return Ok(None);
        };
        if avcc.is_empty() || !self.keyframe_seen {
            return Ok(None);
        }
        // SAFETY: a block buffer over `avcc` (no copy, no free: `avcc` outlives the synchronous
        // decode), one sample of it, decoded synchronously into the boxed slot.
        unsafe {
            let mut block: *mut CMBlockBuffer = null_mut();
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                avcc.as_mut_ptr().cast(),
                avcc.len(),
                Some(objc2_core_foundation::kCFAllocatorNull.expect("kCFAllocatorNull")),
                std::ptr::null(),
                0,
                avcc.len(),
                0,
                NonNull::from(&mut block),
            );
            let block = NonNull::new(block)
                .filter(|_| status == 0)
                .map(|block| CFRetained::<CMBlockBuffer>::from_raw(block))
                .ok_or_else(|| format!("The video frame could not be wrapped ({status})."))?;
            let size = avcc.len();
            let mut sample: *mut CMSampleBuffer = null_mut();
            let status = CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&format.0),
                1,
                0,
                std::ptr::null(),
                1,
                &size,
                NonNull::from(&mut sample),
            );
            let sample = NonNull::new(sample)
                .filter(|_| status == 0)
                .map(|sample| CFRetained::<CMSampleBuffer>::from_raw(sample))
                .ok_or_else(|| format!("The video frame could not be prepared ({status})."))?;
            let mut flags = VTDecodeInfoFlags(0);
            let status = session.0.decode_frame(&sample, VTDecodeFrameFlags(0), null_mut(), &mut flags);
            session.0.wait_for_asynchronous_frames();
            if status != 0 {
                // A broken frame: wait for the next keyframe rather than fail the call.
                log::debug!("tinyclient: H.264 frame not decoded ({status})");
                self.keyframe_seen = false;
                return Ok(None);
            }
        }
        Ok(self.out.lock().ok().and_then(|mut slot| slot.take()))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_on_three_and_four_byte_start_codes() {
        let stream = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let units = nal_units(&stream);
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn avcc_becomes_annex_b_and_reports_an_idr() {
        let avcc = [0, 0, 0, 2, 0x65, 9, 0, 0, 0, 1, 0x41];
        let mut out = Vec::new();
        assert!(avcc_to_annex_b(&avcc, &mut out));
        assert_eq!(out, vec![0, 0, 0, 1, 0x65, 9, 0, 0, 0, 1, 0x41]);
    }
}
