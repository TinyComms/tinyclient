//! The system H.264 MFT with a D3D11 device manager. Accept only GPU-backed NV12 output.
use crate::mf_encoder::{self, Platform};
use crate::platform::{PixelFormat, VideoFrame};
use std::mem::ManuallyDrop;
use std::ptr::{self, null_mut};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{Interface, Result};

pub(crate) struct Decoder {
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    size: (u32, u32),
    aperture: Option<Aperture>,
    time: i64,
    _platform: Platform,
}

impl Decoder {
    pub(crate) fn open() -> std::result::Result<Self, String> {
        let platform = mf_encoder::platform()?;
        // SAFETY: COM and MF remain initialized on this decode worker. The manager owns its
        // device and remains alive until after the MFT is dropped.
        unsafe { Self::start(platform) }
            .map_err(|e| format!("open Media Foundation D3D11 H.264 decoder: {e}"))
    }

    unsafe fn start(platform: Platform) -> Result<Self> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
            let device = device.ok_or_else(invalid)?;
            let _ = device
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(invalid)?;
            manager.ResetDevice(&device, token)?;

            let transform: IMFTransform =
                CoCreateInstance(&CMSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            let attributes = transform.GetAttributes()?;
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE)? == 0 {
                return Err(invalid());
            }
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;
            let mut decoder = Self {
                transform,
                _manager: manager,
                size: (0, 0),
                aperture: None,
                time: 0,
                _platform: platform,
            };
            decoder.output_type()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    fn output_type(&mut self) -> Result<()> {
        // SAFETY: the returned types are owned COM objects. The H.264 MFT has one input and
        // one output with stream ID zero. An initial type may have no size until the SPS.
        unsafe {
            for index in 0.. {
                let output = self.transform.GetOutputAvailableType(0, index)?;
                if output.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                    continue;
                }
                self.transform.SetOutputType(0, &output, 0)?;
                let size = output.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                self.size = ((size >> 32) as u32, size as u32);
                self.aperture = display_aperture(&output);
                return Ok(());
            }
        }
        unreachable!()
    }

    pub(crate) fn decode(
        &mut self,
        annex_b: &[u8],
    ) -> std::result::Result<Option<VideoFrame>, String> {
        // SAFETY: all samples and buffers are owned COM references on this worker.
        unsafe { self.decode_sample(annex_b) }
            .map_err(|e| format!("Media Foundation D3D11 H.264 decode: {e}"))
    }

    unsafe fn decode_sample(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>> {
        unsafe {
            let len = u32::try_from(annex_b.len()).map_err(|_| invalid())?;
            let buffer = MFCreateMemoryBuffer(len)?;
            let mut data = null_mut();
            buffer.Lock(&mut data, None, None)?;
            ptr::copy_nonoverlapping(annex_b.as_ptr(), data, annex_b.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(333_333)?;
            self.time += 333_333;
            self.transform.ProcessInput(0, &sample, 0)?;

            let mut frame = None;
            // The decoder can release multiple reordered frames for one access unit. Drain
            // them before the next ProcessInput and show the newest frame.
            for _ in 0..32 {
                let mut slot = [MFT_OUTPUT_DATA_BUFFER::default()];
                let mut flags = 0;
                let result = self.transform.ProcessOutput(0, &mut slot, &mut flags);
                let sample = ManuallyDrop::take(&mut slot[0].pSample);
                ManuallyDrop::drop(&mut slot[0].pEvents);
                match result {
                    Ok(()) => {
                        let sample = sample.ok_or_else(invalid)?;
                        frame = Some(self.read_frame(&sample)?);
                    }
                    Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(frame),
                    Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.output_type()?,
                    Err(err) => return Err(err),
                }
            }
            Err(invalid())
        }
    }

    fn read_frame(&self, sample: &IMFSample) -> Result<VideoFrame> {
        // SAFETY: verify the MFT wrote a D3D11 NV12 surface, then stage it to CPU through
        // IMF2DBuffer2. The locked allocation bounds both copied planes including padding.
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            let surface = buffer.cast::<IMFDXGIBuffer>()?;
            let mut raw = null_mut();
            surface.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
            if raw.is_null() {
                return Err(invalid());
            }
            let texture = ID3D11Texture2D::from_raw(raw);
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if desc.BindFlags & D3D11_BIND_DECODER.0 as u32 == 0 {
                return Err(invalid());
            }
            let (x, y, width, height) = match &self.aperture {
                Some(area) => (area.x, area.y, area.width, area.height),
                None => (
                    0,
                    0,
                    if self.size.0 > 0 {
                        self.size.0
                    } else {
                        desc.Width
                    },
                    if self.size.1 > 0 {
                        self.size.1
                    } else {
                        desc.Height
                    },
                ),
            };
            let (width, height) = (width & !1, height & !1);
            if width == 0
                || height == 0
                || x.saturating_add(width) > desc.Width
                || y.saturating_add(height) > desc.Height
            {
                return Err(invalid());
            }
            let rows = buffer.cast::<IMF2DBuffer2>()?;
            let (mut first, mut base, mut pitch, mut len) = (null_mut(), null_mut(), 0, 0);
            rows.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut first,
                &mut pitch,
                &mut base,
                &mut len,
            )?;
            let packed = (|| {
                let offset = (first as usize).checked_sub(base as usize)?;
                let (width, height, stride) = (
                    width as usize,
                    height as usize,
                    usize::try_from(pitch).ok()?,
                );
                let luma = offset
                    .checked_add(stride.checked_mul(y as usize)?)?
                    .checked_add(x as usize)?;
                let chroma = offset
                    .checked_add(stride.checked_mul((desc.Height + y / 2) as usize)?)?
                    .checked_add(x as usize)?;
                let end = chroma
                    .checked_add(stride.checked_mul(height / 2 - 1)?)?
                    .checked_add(width)?;
                if base.is_null()
                    || first.is_null()
                    || stride < x as usize + width
                    || end > len as usize
                {
                    return None;
                }
                let mut data = Vec::with_capacity(width * height * 3 / 2);
                for (start, count) in [(luma, height), (chroma, height / 2)] {
                    for row in 0..count {
                        data.extend_from_slice(std::slice::from_raw_parts(
                            base.add(start + row * stride),
                            width,
                        ));
                    }
                }
                Some(data)
            })();
            rows.Unlock2D()?;
            Ok(VideoFrame {
                width,
                height,
                format: PixelFormat::Nv12,
                data: packed.ok_or_else(invalid)?,
                timestamp_us: 0,
            })
        }
    }
}

struct Aperture {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// The H.264 MFT can return a coded size of 1920×1088 for a visible 1920×1080 picture.
fn display_aperture(media_type: &IMFMediaType) -> Option<Aperture> {
    for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
        let mut bytes = [0u8; std::mem::size_of::<MFVideoArea>()];
        let mut written = 0;
        // SAFETY: a media-type blob with the documented MFVideoArea layout. All fields
        // are integers, and the exact byte count is checked before reading the structure.
        let area = unsafe {
            if media_type
                .GetBlob(&key, &mut bytes, Some(&mut written))
                .is_err()
                || written as usize != bytes.len()
            {
                continue;
            }
            bytes.as_ptr().cast::<MFVideoArea>().read_unaligned()
        };
        let (x, y) = (area.OffsetX.value, area.OffsetY.value);
        if x >= 0
            && y >= 0
            && x % 2 == 0
            && y % 2 == 0
            && area.OffsetX.fract == 0
            && area.OffsetY.fract == 0
            && area.Area.cx > 0
            && area.Area.cy > 0
        {
            return Some(Aperture {
                x: x as u32,
                y: y as u32,
                width: area.Area.cx as u32,
                height: area.Area.cy as u32,
            });
        }
    }
    None
}

fn invalid() -> windows::core::Error {
    windows::core::Error::new(
        MF_E_INVALIDMEDIATYPE,
        "Invalid hardware H.264 media type or surface",
    )
}
