//! PipeWire 0.3 struct layouts a `dlopen` of `libpipewire-0.3.so.0` has to match.
//!
//! The sizes are the 64-bit System V layout, which is what every desktop we ship (x86_64 and
//! aarch64, and this Windows machine where the test runs) uses. A 32-bit build still has
//! `repr(C)` structs; the asserts are the 64-bit check. Nothing here loads the library. The
//! constants are what `portal.rs` passes to PipeWire, and that file is compiled only on Linux.
#![cfg_attr(not(all(target_os = "linux", not(target_env = "ohos"))), allow(dead_code))]

use std::os::raw::c_char;

pub(super) const SPA_TYPE_ID: u32 = 3;
pub(super) const SPA_TYPE_RECTANGLE: u32 = 10;
pub(super) const SPA_TYPE_FRACTION: u32 = 11;
pub(super) const SPA_TYPE_OBJECT: u32 = 15;
pub(super) const SPA_TYPE_CHOICE: u32 = 19;
pub(super) const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;

pub(super) const SPA_CHOICE_RANGE: u32 = 1;
pub(super) const SPA_CHOICE_ENUM: u32 = 3;

pub(super) const SPA_PARAM_ENUM_FORMAT: u32 = 3;
pub(super) const SPA_PARAM_FORMAT: u32 = 4;

pub(super) const SPA_MEDIA_TYPE_VIDEO: u32 = 2;
pub(super) const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;

pub(super) const SPA_FORMAT_VIDEO_FORMAT: u32 = 0x20001;
pub(super) const SPA_FORMAT_VIDEO_SIZE: u32 = 0x20003;
pub(super) const SPA_FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;

pub(super) const SPA_VIDEO_FORMAT_RGBX: u32 = 7;
pub(super) const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
pub(super) const SPA_VIDEO_FORMAT_RGBA: u32 = 11;
pub(super) const SPA_VIDEO_FORMAT_BGRA: u32 = 12;
pub(super) const SPA_VIDEO_FORMAT_NV12: u32 = 23;

pub(super) const PW_STREAM_STATE_ERROR: i32 = -1;
pub(super) const PW_STREAM_STATE_STREAMING: i32 = 3;
pub(super) const PW_ID_ANY: u32 = 0xffff_ffff;
pub(super) const PW_STREAM_FLAG_AUTOCONNECT: u32 = 1;
pub(super) const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 4;

#[repr(C)]
pub(super) struct SpaDictItem {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub(super) struct SpaDict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const SpaDictItem,
}

/// `spa_chunk`: offset, then size, then stride. The stride is not the second field.
#[repr(C)]
pub(super) struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
pub(super) struct SpaData {
    pub type_: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut u8,
    pub chunk: *mut SpaChunk,
}

#[repr(C)]
pub(super) struct SpaMeta {
    pub type_: u32,
    pub size: u32,
    pub data: *mut u8,
}

#[repr(C)]
pub(super) struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut SpaMeta,
    pub datas: *mut SpaData,
}

#[repr(C)]
pub(super) struct PwBuffer {
    pub buffer: *mut SpaBuffer,
    pub user_data: *mut std::ffi::c_void,
    pub size: u64,
    pub requested: u64,
    pub time: u64,
}

/// `pw_stream_events` version 2. The four bytes after `version` are padding the C compiler
/// inserts before the first function pointer; Rust inserts the same padding, so it is not a field.
#[repr(C)]
pub(super) struct StreamEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut std::ffi::c_void, i32, i32, *const c_char)>,
    pub control_info:
        Option<unsafe extern "C" fn(*mut std::ffi::c_void, u32, *const std::ffi::c_void)>,
    pub io_changed:
        Option<unsafe extern "C" fn(*mut std::ffi::c_void, u32, *mut std::ffi::c_void, u32)>,
    pub param_changed: Option<unsafe extern "C" fn(*mut std::ffi::c_void, u32, *const u8)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut PwBuffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut PwBuffer)>,
    pub process: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    pub drained: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    pub command: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void)>,
    pub trigger_done: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn pipewire_structs_match_the_64_bit_abi() {
        assert_eq!(size_of::<SpaDict>(), 16);
        assert_eq!(size_of::<SpaChunk>(), 16);
        assert_eq!(std::mem::offset_of!(SpaChunk, stride), 8);
        assert_eq!(size_of::<SpaData>(), 40);
        assert_eq!(std::mem::offset_of!(SpaData, data), 24);
        assert_eq!(size_of::<SpaMeta>(), 16);
        assert_eq!(size_of::<SpaBuffer>(), 24);
        assert_eq!(size_of::<PwBuffer>(), 40);
        assert_eq!(size_of::<StreamEvents>(), 96);
        assert_eq!(std::mem::offset_of!(StreamEvents, destroy), 8);
        assert_eq!(std::mem::offset_of!(StreamEvents, process), 64);
    }
}
