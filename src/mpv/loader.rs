//! Runtime resolution of libmpv.
//!
//! The whole crate links against nothing native. We `dlopen` libmpv on first use and pull the
//! symbols we need out of it. A missing or too-old libmpv becomes a typed error the caller can
//! show a user, instead of a link failure at build time on someone else's machine.

use std::ffi::{c_char, c_int, c_ulong, c_void, OsStr};
use std::sync::OnceLock;

use libloading::{Library, Symbol};

use super::ffi::*;
use crate::error::{Error, Result};

/// Environment variable that overrides library discovery entirely.
pub const LIBMPV_PATH_ENV: &str = "TAURI_PLUGIN_MPV_LIBMPV_PATH";

/// libmpv client API version 1.107 corresponds to mpv 0.33, the first release where the render
/// API is stable in the form we use. Anything older is rejected up front with a clear message.
const MIN_CLIENT_API_MAJOR: u32 = 1;
const MIN_CLIENT_API_MINOR: u32 = 107;

macro_rules! mpv_symbols {
    ($( $field:ident : $sym:literal => unsafe extern "C" fn($($arg:ty),* $(,)?) $( -> $ret:ty )? ; )*) => {
        /// Resolved libmpv entry points.
        pub struct MpvLib {
            // Keeps the dlopen handle alive for the process lifetime; the fn pointers below
            // point into it, so it must outlive them. Never dropped in practice (see `get`).
            _lib: Library,
            $( pub $field: unsafe extern "C" fn($($arg),*) $( -> $ret )?, )*
        }

        impl MpvLib {
            fn from_library(lib: Library) -> Result<Self> {
                unsafe {
                    $(
                        let $field: Symbol<unsafe extern "C" fn($($arg),*) $( -> $ret )?> =
                            lib.get($sym).map_err(|e| Error::MpvSymbolMissing {
                                symbol: String::from_utf8_lossy($sym)
                                    .trim_end_matches('\0')
                                    .to_string(),
                                reason: e.to_string(),
                            })?;
                        let $field = *$field;
                    )*
                    Ok(MpvLib { _lib: lib, $( $field, )* })
                }
            }
        }
    };
}

mpv_symbols! {
    client_api_version: b"mpv_client_api_version\0" => unsafe extern "C" fn() -> c_ulong;
    create: b"mpv_create\0" => unsafe extern "C" fn() -> *mut MpvHandle;
    initialize: b"mpv_initialize\0" => unsafe extern "C" fn(*mut MpvHandle) -> c_int;
    terminate_destroy: b"mpv_terminate_destroy\0" => unsafe extern "C" fn(*mut MpvHandle);
    set_option_string: b"mpv_set_option_string\0" => unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
    command_node: b"mpv_command_node\0" => unsafe extern "C" fn(*mut MpvHandle, *mut MpvNode, *mut MpvNode) -> c_int;
    set_property: b"mpv_set_property\0" => unsafe extern "C" fn(*mut MpvHandle, *const c_char, MpvFormat, *mut c_void) -> c_int;
    get_property: b"mpv_get_property\0" => unsafe extern "C" fn(*mut MpvHandle, *const c_char, MpvFormat, *mut c_void) -> c_int;
    observe_property: b"mpv_observe_property\0" => unsafe extern "C" fn(*mut MpvHandle, u64, *const c_char, MpvFormat) -> c_int;
    unobserve_property: b"mpv_unobserve_property\0" => unsafe extern "C" fn(*mut MpvHandle, u64) -> c_int;
    wait_event: b"mpv_wait_event\0" => unsafe extern "C" fn(*mut MpvHandle, f64) -> *mut MpvEvent;
    wakeup: b"mpv_wakeup\0" => unsafe extern "C" fn(*mut MpvHandle);
    error_string: b"mpv_error_string\0" => unsafe extern "C" fn(c_int) -> *const c_char;
    free: b"mpv_free\0" => unsafe extern "C" fn(*mut c_void);
    free_node_contents: b"mpv_free_node_contents\0" => unsafe extern "C" fn(*mut MpvNode);
    request_log_messages: b"mpv_request_log_messages\0" => unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> c_int;
    render_context_create: b"mpv_render_context_create\0" => unsafe extern "C" fn(*mut *mut MpvRenderContext, *mut MpvHandle, *mut MpvRenderParam) -> c_int;
    render_context_set_update_callback: b"mpv_render_context_set_update_callback\0" => unsafe extern "C" fn(*mut MpvRenderContext, Option<MpvRenderUpdateFn>, *mut c_void);
    render_context_update: b"mpv_render_context_update\0" => unsafe extern "C" fn(*mut MpvRenderContext) -> u64;
    render_context_render: b"mpv_render_context_render\0" => unsafe extern "C" fn(*mut MpvRenderContext, *mut MpvRenderParam) -> c_int;
    render_context_report_swap: b"mpv_render_context_report_swap\0" => unsafe extern "C" fn(*mut MpvRenderContext);
    render_context_free: b"mpv_render_context_free\0" => unsafe extern "C" fn(*mut MpvRenderContext);
}

// The resolved function pointers are plain `extern "C" fn`s into a library that is never
// unloaded, so sharing them across threads is sound. libmpv itself is thread-safe for the calls
// we make (the render-thread rules are enforced a level up, in `render.rs`).
unsafe impl Send for MpvLib {}
unsafe impl Sync for MpvLib {}

static LIB: OnceLock<std::result::Result<MpvLib, Error>> = OnceLock::new();

/// Candidate sonames, tried in order. The first that both loads and yields every symbol wins.
fn candidates() -> Vec<String> {
    if let Some(explicit) = std::env::var_os(LIBMPV_PATH_ENV) {
        return vec![explicit.to_string_lossy().into_owned()];
    }

    #[cfg(target_os = "windows")]
    let names = vec![
        "libmpv-2.dll".to_string(),
        "mpv-2.dll".to_string(),
        "libmpv.dll".to_string(),
        "mpv-1.dll".to_string(),
    ];

    #[cfg(target_os = "macos")]
    let names = vec![
        "libmpv.2.dylib".to_string(),
        "libmpv.dylib".to_string(),
        // Homebrew does not put its lib dir on the default dyld search path.
        "/opt/homebrew/lib/libmpv.2.dylib".to_string(),
        "/opt/homebrew/lib/libmpv.dylib".to_string(),
        "/usr/local/lib/libmpv.2.dylib".to_string(),
        "/usr/local/lib/libmpv.dylib".to_string(),
    ];

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let names = vec![
        "libmpv.so.2".to_string(),
        "libmpv.so.1".to_string(),
        "libmpv.so".to_string(),
    ];

    names
}

fn load() -> Result<MpvLib> {
    let mut attempts = Vec::new();

    for name in candidates() {
        match unsafe { Library::new(OsStr::new(&name)) } {
            Ok(lib) => match MpvLib::from_library(lib) {
                Ok(loaded) => {
                    let version = unsafe { (loaded.client_api_version)() } as u32;
                    let (major, minor) = (version >> 16, version & 0xffff);
                    if (major, minor) < (MIN_CLIENT_API_MAJOR, MIN_CLIENT_API_MINOR) {
                        return Err(Error::MpvTooOld {
                            found: format!("{major}.{minor}"),
                            required: format!("{MIN_CLIENT_API_MAJOR}.{MIN_CLIENT_API_MINOR}"),
                        });
                    }
                    log::info!("loaded libmpv from {name} (client API {major}.{minor})");
                    return Ok(loaded);
                }
                Err(e) => attempts.push(format!("{name}: {e}")),
            },
            Err(e) => attempts.push(format!("{name}: {e}")),
        }
    }

    Err(Error::MpvNotFound {
        attempts: attempts.join("; "),
    })
}

/// Resolve libmpv, loading it on first call. Subsequent calls are cheap.
pub fn get() -> Result<&'static MpvLib> {
    match LIB.get_or_init(load) {
        Ok(lib) => Ok(lib),
        Err(e) => Err(e.clone()),
    }
}

/// Turn a libmpv negative return code into a typed error.
pub fn check(lib: &MpvLib, code: c_int) -> Result<()> {
    if code >= MPV_ERROR_SUCCESS {
        return Ok(());
    }
    let msg = unsafe {
        let ptr = (lib.error_string)(code);
        if ptr.is_null() {
            "unknown error".to_string()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    Err(Error::Mpv { code, message: msg })
}
