//! Hand-written FFI for the subset of `client.h` and `render.h` this plugin uses.
//!
//! Deliberately *not* generated with bindgen and *not* linked at build time: symbols are
//! resolved from the system libmpv at runtime via `libloading`. That keeps consumers free of
//! any build-time native dependency (no pkg-config, no `.lib`, no clang), and it is what lets
//! CI compile the Windows and macOS backends on machines with no mpv installed at all.
//!
//! Layouts here mirror the upstream headers exactly. Changing them without checking the header
//! is how you get silent memory corruption, so each struct notes its origin.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

// ---------------------------------------------------------------------------
// client.h — opaque handles
// ---------------------------------------------------------------------------

/// Opaque `mpv_handle`.
#[repr(C)]
pub struct MpvHandle {
    _private: [u8; 0],
}

/// Opaque `mpv_render_context`.
#[repr(C)]
pub struct MpvRenderContext {
    _private: [u8; 0],
}

// ---------------------------------------------------------------------------
// client.h — mpv_format
// ---------------------------------------------------------------------------

pub type MpvFormat = c_int;

pub const MPV_FORMAT_NONE: MpvFormat = 0;
pub const MPV_FORMAT_STRING: MpvFormat = 1;
pub const MPV_FORMAT_OSD_STRING: MpvFormat = 2;
pub const MPV_FORMAT_FLAG: MpvFormat = 3;
pub const MPV_FORMAT_INT64: MpvFormat = 4;
pub const MPV_FORMAT_DOUBLE: MpvFormat = 5;
pub const MPV_FORMAT_NODE: MpvFormat = 6;
pub const MPV_FORMAT_NODE_ARRAY: MpvFormat = 7;
pub const MPV_FORMAT_NODE_MAP: MpvFormat = 8;
pub const MPV_FORMAT_BYTE_ARRAY: MpvFormat = 9;

// ---------------------------------------------------------------------------
// client.h — mpv_node
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub union MpvNodeUnion {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: f64,
    pub list: *mut MpvNodeList,
    pub ba: *mut MpvByteArray,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvNode {
    pub u: MpvNodeUnion,
    pub format: MpvFormat,
}

impl MpvNode {
    /// A `MPV_FORMAT_NONE` node, safe to pass as an empty result out-param.
    pub fn none() -> Self {
        MpvNode {
            u: MpvNodeUnion { int64: 0 },
            format: MPV_FORMAT_NONE,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvNodeList {
    pub num: c_int,
    pub values: *mut MpvNode,
    /// NULL for arrays, populated for maps.
    pub keys: *mut *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvByteArray {
    pub data: *mut c_void,
    pub size: usize,
}

// ---------------------------------------------------------------------------
// client.h — events
// ---------------------------------------------------------------------------

pub type MpvEventId = c_int;

pub const MPV_EVENT_NONE: MpvEventId = 0;
pub const MPV_EVENT_SHUTDOWN: MpvEventId = 1;
pub const MPV_EVENT_LOG_MESSAGE: MpvEventId = 2;
pub const MPV_EVENT_GET_PROPERTY_REPLY: MpvEventId = 3;
pub const MPV_EVENT_SET_PROPERTY_REPLY: MpvEventId = 4;
pub const MPV_EVENT_COMMAND_REPLY: MpvEventId = 5;
pub const MPV_EVENT_START_FILE: MpvEventId = 6;
pub const MPV_EVENT_END_FILE: MpvEventId = 7;
pub const MPV_EVENT_FILE_LOADED: MpvEventId = 8;
pub const MPV_EVENT_SEEK: MpvEventId = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: MpvEventId = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: MpvEventId = 22;
pub const MPV_EVENT_QUEUE_OVERFLOW: MpvEventId = 24;

#[repr(C)]
pub struct MpvEvent {
    pub event_id: MpvEventId,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// `mpv_event_property`, pointed to by `MpvEvent::data` for `MPV_EVENT_PROPERTY_CHANGE`.
#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: MpvFormat,
    pub data: *mut c_void,
}

/// `mpv_event_log_message`, pointed to by `MpvEvent::data` for `MPV_EVENT_LOG_MESSAGE`.
#[repr(C)]
pub struct MpvEventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

/// `mpv_event_end_file`, pointed to by `MpvEvent::data` for `MPV_EVENT_END_FILE`.
#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;

// ---------------------------------------------------------------------------
// render.h
// ---------------------------------------------------------------------------

pub type MpvRenderParamType = c_int;

pub const MPV_RENDER_PARAM_INVALID: MpvRenderParamType = 0;
pub const MPV_RENDER_PARAM_API_TYPE: MpvRenderParamType = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: MpvRenderParamType = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: MpvRenderParamType = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: MpvRenderParamType = 4;
pub const MPV_RENDER_PARAM_DEPTH: MpvRenderParamType = 5;
pub const MPV_RENDER_PARAM_ADVANCED_CONTROL: MpvRenderParamType = 10;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: MpvRenderParamType = 12;
pub const MPV_RENDER_PARAM_SKIP_RENDERING: MpvRenderParamType = 13;

/// `MPV_RENDER_API_TYPE_OPENGL`
pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";

/// Bit returned by `mpv_render_context_update`: a new frame is ready to be drawn.
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvRenderParam {
    pub type_: MpvRenderParamType,
    pub data: *mut c_void,
}

impl MpvRenderParam {
    pub fn terminator() -> Self {
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_INVALID,
            data: std::ptr::null_mut(),
        }
    }
}

pub type GetProcAddressFn =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;

#[repr(C)]
pub struct MpvOpenGLInitParams {
    pub get_proc_address: Option<GetProcAddressFn>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpvOpenGLFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub type MpvRenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

// ---------------------------------------------------------------------------
// Error codes we care about by name
// ---------------------------------------------------------------------------

pub const MPV_ERROR_SUCCESS: c_int = 0;
pub const MPV_ERROR_PROPERTY_NOT_FOUND: c_int = -8;
pub const MPV_ERROR_PROPERTY_UNAVAILABLE: c_int = -10;
