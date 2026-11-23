//! Runtime FFmpeg VA-API and NVENC. No FFmpeg or GPU driver is needed to link or launch mediad.
use crate::platform::{EncoderConfig, H264Decoder, PixelFormat, VideoFrame};
use libloading::Library;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;

type Ptr = *mut c_void;
const AGAIN: c_int = -libc::EAGAIN;
const LOW_DELAY: c_int = 1 << 19;
const CONSTRAINED_BASELINE: c_int = 66 | (1 << 9);
const VAAPI: &str = "VA-API";
const NVENC: &str = "NVENC";

// Only these fields of AVCodecContext and AVHWFramesContext are needed. Offsets come from
// offsetof against the upstream 4.4, 5.1, 6.1, 7.1 and 8.0 public headers on Linux LP64.
// libavcodec/libavutil promise a stable public ABI within a major. Never open an unknown major,
// or mix libavutil from another major. The layout is the same on Linux x86_64 and aarch64.
struct Layout {
    width: usize,
    height: usize,
    pixel: usize,
    clock: usize,
    rate: usize,
    bps: usize,
    gop: usize,
    b_frames: usize,
    profile: usize,
    flags: usize,
    device: usize,
    frames: usize,
    get_format: usize,
    threads: usize,
    refs: usize,
    hw_format: usize,
    picture_type: usize,
}

const LAYOUTS: [(u32, u32, Layout); 5] = [
    (
        58,
        56,
        Layout {
            width: 116,
            height: 120,
            pixel: 136,
            clock: 100,
            rate: 872,
            bps: 56,
            gop: 132,
            b_frames: 160,
            profile: 820,
            flags: 76,
            device: 1032,
            frames: 1008,
            get_format: 152,
            threads: 784,
            refs: 356,
            hw_format: 68,
            picture_type: 124,
        },
    ),
    (
        59,
        57,
        Layout {
            width: 116,
            height: 120,
            pixel: 136,
            clock: 100,
            rate: 712,
            bps: 56,
            gop: 132,
            b_frames: 160,
            profile: 676,
            flags: 76,
            device: 872,
            frames: 848,
            get_format: 152,
            threads: 636,
            refs: 316,
            hw_format: 68,
            picture_type: 124,
        },
    ),
    (
        60,
        58,
        Layout {
            width: 116,
            height: 120,
            pixel: 136,
            clock: 100,
            rate: 704,
            bps: 56,
            gop: 132,
            b_frames: 160,
            profile: 668,
            flags: 76,
            device: 864,
            frames: 840,
            get_format: 152,
            threads: 636,
            refs: 316,
            hw_format: 68,
            picture_type: 124,
        },
    ),
    (
        61,
        59,
        Layout {
            width: 116,
            height: 120,
            pixel: 140,
            clock: 84,
            rate: 100,
            bps: 56,
            gop: 332,
            b_frames: 200,
            profile: 688,
            flags: 64,
            device: 560,
            frames: 552,
            get_format: 192,
            threads: 656,
            refs: 172,
            hw_format: 60,
            picture_type: 124,
        },
    ),
    (
        62,
        60,
        Layout {
            width: 112,
            height: 116,
            pixel: 136,
            clock: 84,
            rate: 100,
            bps: 56,
            gop: 332,
            b_frames: 200,
            profile: 688,
            flags: 64,
            device: 560,
            frames: 552,
            get_format: 192,
            threads: 656,
            refs: 168,
            hw_format: 60,
            picture_type: 120,
        },
    ),
];

#[repr(C)]
struct Frame {
    data: [*mut u8; 8],
    stride: [c_int; 8],
    extended_data: Ptr,
    width: c_int,
    height: c_int,
    samples: c_int,
    format: c_int,
}

#[repr(C)]
struct Packet {
    buffer: Ptr,
    pts: i64,
    dts: i64,
    data: *mut u8,
    size: c_int,
}

macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $out:ty)?;)*) => {
        struct Api {
            _codec: Library,
            layout: &'static Layout,
            $($name: unsafe extern "C" fn($($arg),*) $(-> $out)?,)*
        }

        impl Api {
            unsafe fn load(codec: Library, layout: &'static Layout) -> Result<Self, libloading::Error> {
                Ok(Self {
                    $($name: unsafe { *codec.get(concat!(stringify!($name), "\0").as_bytes())? },)*
                    _codec: codec, layout,
                })
            }
        }
    }
}

api! {
    avcodec_find_encoder_by_name: fn(*const c_char) -> Ptr;
    avcodec_find_decoder_by_name: fn(*const c_char) -> Ptr;
    avcodec_get_hw_config: fn(Ptr, c_int) -> *const HwConfig;
    avcodec_alloc_context3: fn(Ptr) -> Ptr;
    avcodec_free_context: fn(*mut Ptr);
    avcodec_open2: fn(Ptr, Ptr, *mut Ptr) -> c_int;
    avcodec_send_frame: fn(Ptr, Ptr) -> c_int;
    avcodec_receive_packet: fn(Ptr, Ptr) -> c_int;
    avcodec_send_packet: fn(Ptr, Ptr) -> c_int;
    avcodec_receive_frame: fn(Ptr, Ptr) -> c_int;
    av_packet_alloc: fn() -> Ptr;
    av_packet_free: fn(*mut Ptr);
    av_packet_unref: fn(Ptr);
    av_new_packet: fn(Ptr, c_int) -> c_int;
    av_frame_alloc: fn() -> Ptr;
    av_frame_free: fn(*mut Ptr);
    av_frame_unref: fn(Ptr);
    av_frame_get_buffer: fn(Ptr, c_int) -> c_int;
    av_frame_make_writable: fn(Ptr) -> c_int;
    av_hwdevice_find_type_by_name: fn(*const c_char) -> c_int;
    av_hwdevice_ctx_create: fn(*mut Ptr, c_int, *const c_char, Ptr, c_int) -> c_int;
    av_hwframe_ctx_alloc: fn(Ptr) -> Ptr;
    av_hwframe_ctx_init: fn(Ptr) -> c_int;
    av_hwframe_get_buffer: fn(Ptr, Ptr, c_int) -> c_int;
    av_hwframe_transfer_data: fn(Ptr, Ptr, c_int) -> c_int;
    av_get_pix_fmt: fn(*const c_char) -> c_int;
    av_opt_set: fn(Ptr, *const c_char, *const c_char, c_int) -> c_int;
    av_strerror: fn(c_int, *mut c_char, usize) -> c_int;
}

#[repr(C)]
struct HwConfig {
    pixel: c_int,
    methods: c_int,
    device: c_int,
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        for (avcodec, avutil, layout) in LAYOUTS.iter().rev() {
            // SAFETY: system FFmpeg libraries, versioned by SONAME. Resolve utility symbols
            // from the codec's own dependency scope so allocations always use the same libavutil.
            let found = unsafe {
                (|| {
                    let codec = Library::new(format!("libavcodec.so.{avcodec}")).ok()?;
                    type Version = unsafe extern "C" fn() -> u32;
                    let codec_version = codec.get::<Version>(b"avcodec_version\0").ok()?;
                    let util_version = codec.get::<Version>(b"avutil_version\0").ok()?;
                    if codec_version() >> 16 != *avcodec || util_version() >> 16 != *avutil {
                        return None;
                    }
                    Api::load(codec, layout).ok()
                })()
            };
            if found.is_some() {
                return found;
            }
        }
        None
    })
    .as_ref()
    .ok_or_else(|| "No supported system libavcodec/libavutil pair was found.".into())
}

impl Api {
    fn check(&self, operation: &str, status: c_int) -> Result<(), String> {
        if status >= 0 {
            return Ok(());
        }
        let mut words = [0 as c_char; 128];
        // SAFETY: FFmpeg writes a terminated error description into the supplied buffer.
        unsafe {
            (self.av_strerror)(status, words.as_mut_ptr(), words.len());
            Err(format!(
                "{operation}: {}",
                CStr::from_ptr(words.as_ptr()).to_string_lossy()
            ))
        }
    }
}

struct Resources {
    api: &'static Api,
    context: Ptr,
    cpu: Ptr,
    gpu: Ptr,
    packet: Ptr,
}

// SAFETY: exclusively owned AV objects, with no thread-local state in our use. FFmpeg manages
// any threads inside the codec. This is moved only as part of a decoder passed to a worker.
unsafe impl Send for Resources {}

impl Resources {
    fn new(api: &'static Api, codec: Ptr) -> Result<Self, String> {
        if codec.is_null() {
            return Err("This FFmpeg build lacks the requested H.264 codec.".into());
        }
        // SAFETY: allocate exclusively owned FFmpeg objects and free them in Drop.
        let found = unsafe {
            Self {
                api,
                context: (api.avcodec_alloc_context3)(codec),
                cpu: (api.av_frame_alloc)(),
                gpu: (api.av_frame_alloc)(),
                packet: (api.av_packet_alloc)(),
            }
        };
        if [found.context, found.cpu, found.gpu, found.packet]
            .iter()
            .any(|p| p.is_null())
        {
            return Err("FFmpeg could not allocate the video codec.".into());
        }
        Ok(found)
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.av_frame_free)(&mut self.gpu);
            (self.api.av_frame_free)(&mut self.cpu);
            (self.api.avcodec_free_context)(&mut self.context);
        }
    }
}

// SAFETY: callers supply an aligned field offset for the loaded library's ABI and an initialized
// object of the corresponding type. FFmpeg owns these allocations, including any buffer refs
// transferred into its context.
unsafe fn field<T>(object: Ptr, offset: usize) -> *mut T {
    unsafe { object.cast::<u8>().add(offset).cast() }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EncodeKind {
    Vaapi,
    Nvenc,
}

pub(crate) struct Encoder {
    res: Resources,
    kind: EncodeKind,
    config: EncoderConfig,
    frame: i64,
    waiting: u8,
    first: bool,
    last_rate: std::time::Instant,
}

impl Encoder {
    pub(crate) fn open(config: EncoderConfig) -> Result<Self, String> {
        let vaapi = Self::new(config, EncodeKind::Vaapi);
        match vaapi {
            Ok(encoder) => Ok(encoder),
            Err(vaapi) => Self::new(config, EncodeKind::Nvenc)
                .map_err(|nvenc| format!("VA-API: {vaapi}; NVENC: {nvenc}")),
        }
    }

    fn new(config: EncoderConfig, kind: EncodeKind) -> Result<Self, String> {
        let api = api()?;
        let name = match kind {
            EncodeKind::Vaapi => c"h264_vaapi",
            EncodeKind::Nvenc => c"h264_nvenc",
        };
        // SAFETY: the selected libavcodec/libavutil ABI determines every field offset. The
        // context exclusively owns its device and frame references after they are installed.
        unsafe {
            let codec = (api.avcodec_find_encoder_by_name)(name.as_ptr());
            let res = Resources::new(api, codec)?;
            let ctx = res.context;
            let l = api.layout;
            let fps = config.fps.max(1) as c_int;
            let nv12 = (api.av_get_pix_fmt)(c"nv12".as_ptr());
            let mut pixel = nv12;
            if kind == EncodeKind::Vaapi {
                let vaapi = (api.av_hwdevice_find_type_by_name)(c"vaapi".as_ptr());
                api.check(
                    "open VA-API device",
                    (api.av_hwdevice_ctx_create)(
                        field(ctx, l.device),
                        vaapi,
                        ptr::null(),
                        ptr::null_mut(),
                        0,
                    ),
                )?;
                let frames = (api.av_hwframe_ctx_alloc)(*field(ctx, l.device));
                let frames = NonNull::new(frames)
                    .ok_or("VA-API could not allocate frame surfaces.")?
                    .as_ptr();
                *field::<Ptr>(ctx, l.frames) = frames;
                let hw = *field::<Ptr>(frames, 8); // AVBufferRef.data
                pixel = (api.av_get_pix_fmt)(c"vaapi".as_ptr());
                *field::<c_int>(hw, l.hw_format) = pixel;
                *field::<c_int>(hw, l.hw_format + 4) = nv12;
                *field::<c_int>(hw, l.hw_format + 8) = config.width as c_int;
                *field::<c_int>(hw, l.hw_format + 12) = config.height as c_int;
                *field::<c_int>(hw, l.hw_format - 4) = 16;
            }
            *field::<c_int>(ctx, l.width) = config.width as c_int;
            *field::<c_int>(ctx, l.height) = config.height as c_int;
            *field::<c_int>(ctx, l.pixel) = pixel;
            *field::<[c_int; 2]>(ctx, l.clock) = [1, fps];
            *field::<[c_int; 2]>(ctx, l.rate) = [fps, 1];
            *field::<i64>(ctx, l.bps) = i64::from(config.bitrate_bps);
            *field::<c_int>(ctx, l.gop) = fps * 4;
            *field::<c_int>(ctx, l.b_frames) = 0;
            *field::<c_int>(ctx, l.refs) = 1;
            *field::<c_int>(ctx, l.flags) |= LOW_DELAY;

            let private = *field::<Ptr>(ctx, 32); // AVCodecContext.priv_data
            let set = |key: &CStr, value: &CStr| {
                (api.av_opt_set)(private, key.as_ptr(), value.as_ptr(), 0)
            };
            match kind {
                EncodeKind::Vaapi => {
                    *field::<c_int>(ctx, l.profile) = CONSTRAINED_BASELINE;
                    let _ = set(c"async_depth", c"1"); // Absent on older FFmpeg.
                    api.check(
                        "initialise VA-API surfaces",
                        (api.av_hwframe_ctx_init)(*field(ctx, l.frames)),
                    )?;
                }
                EncodeKind::Nvenc => {
                    api.check("enable forced IDR", set(c"forced-idr", c"1"))?;
                    api.check("select baseline profile", set(c"profile", c"baseline"))?;
                    api.check("enable low latency", set(c"zerolatency", c"1"))?;
                    api.check("disable output delay", set(c"delay", c"0"))?;
                }
            }
            api.check(
                "open H.264 hardware encoder",
                (api.avcodec_open2)(ctx, codec, ptr::null_mut()),
            )?;
            let cpu = &mut *res.cpu.cast::<Frame>();
            cpu.width = config.width as c_int;
            cpu.height = config.height as c_int;
            cpu.format = nv12;
            api.check(
                "allocate NV12 frame",
                (api.av_frame_get_buffer)(res.cpu, 32),
            )?;
            Ok(Self {
                res,
                kind,
                config,
                frame: 0,
                waiting: 0,
                first: true,
                last_rate: std::time::Instant::now(),
            })
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self.kind {
            EncodeKind::Vaapi => VAAPI,
            EncodeKind::Nvenc => NVENC,
        }
    }

    pub(crate) fn encode(
        &mut self,
        frame: &VideoFrame,
        key: bool,
        bps: u32,
        fps: u32,
    ) -> Result<Vec<u8>, String> {
        // VA-API's FFmpeg encoder sets its rate control at open. Reopen at a new rate after
        // a material change or a keyframe request, with a floor on how often we do that.
        let varied = bps.abs_diff(self.config.bitrate_bps) >= self.config.bitrate_bps / 8
            || fps != self.config.fps;
        if (varied || (key && bps != self.config.bitrate_bps))
            && self.last_rate.elapsed() >= std::time::Duration::from_secs(1)
        {
            *self = Self::new(
                EncoderConfig {
                    bitrate_bps: bps.max(1),
                    fps: fps.max(1),
                    ..self.config
                },
                self.kind,
            )?;
        }
        let Resources {
            api,
            context,
            cpu,
            gpu,
            packet,
        } = self.res;
        // SAFETY: frame rows and strides come from FFmpeg's owned NV12 allocation. Packets
        // remain alive until copied. The hardware frame references are owned by FFmpeg.
        unsafe {
            api.check(
                "make NV12 frame writable",
                (api.av_frame_make_writable)(cpu),
            )?;
            let layout = &mut *cpu.cast::<Frame>();
            let width = frame.width as usize;
            let height = frame.height as usize;
            for (plane, rows, start) in [(0, height, 0), (1, height / 2, width * height)] {
                let stride = layout.stride[plane] as usize;
                if stride < width
                    || layout.data[plane].is_null()
                    || frame.data.len() < start + rows * width
                {
                    return Err("FFmpeg encoder received an invalid NV12 layout.".into());
                }
                for row in 0..rows {
                    ptr::copy_nonoverlapping(
                        frame.data.as_ptr().add(start + row * width),
                        layout.data[plane].add(row * stride),
                        width,
                    );
                }
            }
            let input = if self.kind == EncodeKind::Vaapi {
                (api.av_frame_unref)(gpu);
                api.check(
                    "allocate VA-API surface",
                    (api.av_hwframe_get_buffer)(*field(context, api.layout.frames), gpu, 0),
                )?;
                api.check(
                    "upload VA-API surface",
                    (api.av_hwframe_transfer_data)(gpu, cpu, 0),
                )?;
                gpu
            } else {
                cpu
            };
            *field::<i64>(input, 136) = self.frame; // AVFrame.pts
            *field::<c_int>(input, api.layout.picture_type) = if key || self.first { 1 } else { 0 };
            self.frame += 1;
            api.check(
                "send H.264 hardware frame",
                (api.avcodec_send_frame)(context, input),
            )?;
            let result = (api.avcodec_receive_packet)(context, packet);
            if result == AGAIN && self.waiting < 8 {
                self.waiting += 1;
                return Ok(Vec::new());
            }
            api.check("receive H.264 hardware access unit", result)?;
            self.waiting = 0;
            let out = &*packet.cast::<Packet>();
            let bytes = if out.size > 0 && !out.data.is_null() {
                std::slice::from_raw_parts(out.data, out.size as usize).to_vec()
            } else {
                Vec::new()
            };
            (api.av_packet_unref)(packet);
            if !bytes.is_empty() && self.first {
                // The call negotiates constrained baseline. Verify the driver and in-band
                // headers before sending anything, including after a rate-control reopen.
                let units = crate::h264::split(&bytes);
                if !units
                    .iter()
                    .any(|n| n.len() > 3 && n[0] & 31 == 7 && n[1] == 66 && n[2] & 0x40 != 0)
                    || !units.iter().any(|n| n[0] & 31 == 8)
                    || !units.iter().any(|n| n[0] & 31 == 5)
                {
                    return Err(
                        "FFmpeg did not produce a constrained-baseline IDR with in-band headers."
                            .into(),
                    );
                }
                self.first = false;
            }
            Ok(bytes)
        }
    }
}

#[derive(Clone, Copy)]
enum DecodeKind {
    Vaapi,
    Nvdec,
}

pub(crate) struct Decoder {
    res: Resources,
    kind: DecodeKind,
}

type GetFormat = unsafe extern "C" fn(Ptr, *const c_int) -> c_int;

unsafe extern "C" fn vaapi_format(_: Ptr, candidates: *const c_int) -> c_int {
    unsafe { hardware_format(c"vaapi", candidates) }
}

unsafe extern "C" fn cuda_format(_: Ptr, candidates: *const c_int) -> c_int {
    unsafe { hardware_format(c"cuda", candidates) }
}

// Select only the device's pixel format. If it cannot decode this stream, make FFmpeg
// fail explicitly so mediad can switch to OpenH264.
unsafe fn hardware_format(name: &CStr, mut candidates: *const c_int) -> c_int {
    let Ok(api) = api() else { return -1 };
    unsafe {
        let expected = (api.av_get_pix_fmt)(name.as_ptr());
        while *candidates != -1 {
            if *candidates == expected {
                return expected;
            }
            candidates = candidates.add(1);
        }
    }
    -1
}

impl Decoder {
    pub(crate) fn open() -> Result<Self, String> {
        match Self::new(DecodeKind::Vaapi) {
            Ok(decoder) => Ok(decoder),
            Err(vaapi) => Self::new(DecodeKind::Nvdec)
                .map_err(|nvdec| format!("VA-API: {vaapi}; NVDEC: {nvdec}")),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self.kind {
            DecodeKind::Vaapi => VAAPI,
            DecodeKind::Nvdec => "NVDEC",
        }
    }

    fn new(kind: DecodeKind) -> Result<Self, String> {
        let api = api()?;
        let (name, get_format): (&CStr, GetFormat) = match kind {
            DecodeKind::Vaapi => (c"vaapi", vaapi_format),
            DecodeKind::Nvdec => (c"cuda", cuda_format),
        };
        // SAFETY: see Encoder::new. The get_format callback only selects formats from the
        // sentinel-terminated list FFmpeg passes in.
        unsafe {
            let codec = (api.avcodec_find_decoder_by_name)(c"h264".as_ptr());
            let res = Resources::new(api, codec)?;
            let device = (api.av_hwdevice_find_type_by_name)(name.as_ptr());
            let supported = (0..)
                .map(|n| (api.avcodec_get_hw_config)(codec, n))
                .take_while(|config| !config.is_null())
                .any(|config| (*config).device == device && (*config).methods & 1 != 0);
            if !supported {
                return Err("FFmpeg has no H.264 decoder for this device.".into());
            }
            let l = api.layout;
            api.check(
                "open hardware decode device",
                (api.av_hwdevice_ctx_create)(
                    field(res.context, l.device),
                    device,
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                ),
            )?;
            *field::<GetFormat>(res.context, l.get_format) = get_format;
            *field::<c_int>(res.context, l.threads) = 1;
            *field::<c_int>(res.context, l.flags) |= LOW_DELAY;
            api.check(
                "open hardware H.264 decoder",
                (api.avcodec_open2)(res.context, codec, ptr::null_mut()),
            )?;
            Ok(Self { res, kind })
        }
    }
}

impl H264Decoder for Decoder {
    fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String> {
        let Resources {
            api,
            context,
            cpu,
            gpu,
            packet,
        } = self.res;
        let size = c_int::try_from(annex_b.len()).map_err(|_| "H.264 access unit is too large.")?;
        unsafe {
            api.check(
                "allocate received H.264 access unit",
                (api.av_new_packet)(packet, size),
            )?;
            let data = (*packet.cast::<Packet>()).data;
            ptr::copy_nonoverlapping(annex_b.as_ptr(), data, annex_b.len());
            let sent = (api.avcodec_send_packet)(context, packet);
            (api.av_packet_unref)(packet);
            api.check("submit H.264 access unit", sent)?;
            let result = (api.avcodec_receive_frame)(context, gpu);
            if result == AGAIN {
                return Ok(None);
            }
            api.check("decode hardware H.264", result)?;
            let result = (api.av_hwframe_transfer_data)(cpu, gpu, 0);
            (api.av_frame_unref)(gpu);
            api.check("download hardware surface", result)?;
            let frame = &*cpu.cast::<Frame>();
            let copied = unpack(api, frame);
            (api.av_frame_unref)(cpu);
            copied.map(Some)
        }
    }
}

// SAFETY: all plane pointers and strides come from a downloaded AVFrame and remain valid
// until the caller unrefs it.
unsafe fn unpack(api: &Api, frame: &Frame) -> Result<VideoFrame, String> {
    let format = unsafe {
        if frame.format == (api.av_get_pix_fmt)(c"nv12".as_ptr()) {
            PixelFormat::Nv12
        } else if frame.format == (api.av_get_pix_fmt)(c"yuv420p".as_ptr()) {
            PixelFormat::I420
        } else {
            return Err("FFmpeg returned an unsupported frame format.".into());
        }
    };
    if !(2..=8192).contains(&frame.width) || !(2..=8192).contains(&frame.height) {
        return Err("FFmpeg returned invalid dimensions.".into());
    }
    let (width, height) = (frame.width as usize & !1, frame.height as usize & !1);
    let mut data = Vec::with_capacity(width * height * 3 / 2);
    let planes = if format == PixelFormat::Nv12 { 2 } else { 3 };
    for plane in 0..planes {
        let cols = if plane == 0 || planes == 2 {
            width
        } else {
            width / 2
        };
        let rows = if plane == 0 { height } else { height / 2 };
        let stride = frame.stride[plane] as isize;
        if stride.unsigned_abs() < cols || frame.data[plane].is_null() {
            return Err("FFmpeg returned an invalid frame layout.".into());
        }
        for row in 0..rows {
            let data_row = unsafe {
                std::slice::from_raw_parts(frame.data[plane].offset(row as isize * stride), cols)
            };
            data.extend_from_slice(data_row);
        }
    }
    Ok(VideoFrame {
        width: width as u32,
        height: height as u32,
        format,
        data,
        timestamp_us: 0,
    })
}
