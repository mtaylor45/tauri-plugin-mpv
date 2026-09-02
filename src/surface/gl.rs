//! Minimal GL entry-point resolution, in the same runtime-loading style as libmpv.
//!
//! Two resolvers, tried in order:
//!
//! 1. **glXGetProcAddressARB / eglGetProcAddress** — the canonical GL loaders, and what mpv's own
//!    example integrations use. They are plain functions that resolve any entry point by name.
//! 2. **libepoxy** — GTK's own loader, as a fallback.
//!
//! Order matters, and the reason is worth recording. libepoxy exports no
//! `epoxy_get_proc_address`; each entry point is instead a *data* symbol (`epoxy_glFoo`) holding
//! a self-resolving dispatch thunk. Handing those thunks to mpv segfaults inside
//! `mpv_render_context_create`, because they depend on epoxy's per-context dispatch state rather
//! than resolving against the context that is merely current. The GLX/EGL loaders have no such
//! dependency, so they go first.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

use crate::error::{Error, Result};

pub const GL_FRAMEBUFFER_BINDING: c_uint = 0x8CA6;

type ProcResolver = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GlGetIntegerv = unsafe extern "C" fn(c_uint, *mut c_int);

pub struct GlProcs {
    epoxy: Option<Library>,
    /// glXGetProcAddressARB / eglGetProcAddress, whichever loaded.
    loader: Option<(Library, ProcResolver)>,
}

// The libraries are never unloaded and the resolvers are reentrant.
unsafe impl Send for GlProcs {}
unsafe impl Sync for GlProcs {}

static PROCS: OnceLock<std::result::Result<GlProcs, Error>> = OnceLock::new();

impl GlProcs {
    /// Resolve a GL entry point by name.
    ///
    /// # Safety
    /// A GL context must be current on the calling thread.
    pub unsafe fn get(&self, name: &CStr) -> *mut c_void {
        if let Some((_, resolver)) = &self.loader {
            let ptr = resolver(name.as_ptr());
            if !ptr.is_null() {
                return ptr;
            }
        }
        // wglGetProcAddress returns NULL for core GL 1.1 functions; they are plain exports.
        if let Some((lib, _)) = &self.loader {
            let mut symbol = name.to_bytes().to_vec();
            symbol.push(0);
            if let Ok(sym) = lib.get::<*mut c_void>(&symbol) {
                let ptr = *sym;
                if !ptr.is_null() {
                    return ptr;
                }
            }
        }
        if let Some(epoxy) = &self.epoxy {
            // epoxy exports `epoxy_glFoo` as a pointer-sized variable holding a dispatch thunk.
            let mut symbol = Vec::with_capacity(name.to_bytes().len() + 8);
            symbol.extend_from_slice(b"epoxy_");
            symbol.extend_from_slice(name.to_bytes());
            symbol.push(0);
            if let Ok(sym) = epoxy.get::<*mut c_void>(&symbol) {
                let ptr = *sym;
                if !ptr.is_null() {
                    return ptr;
                }
            }
        }
        std::ptr::null_mut()
    }
}

#[cfg(windows)]
fn platform_loader() -> Option<(Library, ProcResolver)> {
    // wglGetProcAddress resolves extensions but returns NULL for GL 1.1 core entry points, which
    // live directly in opengl32.dll. `GlProcs::get` falls through to the library lookup for those.
    let lib = unsafe { Library::new("opengl32.dll") }.ok()?;
    let resolver = unsafe { lib.get::<ProcResolver>(b"wglGetProcAddress\0") }
        .ok()
        .map(|s| *s)?;
    Some((lib, resolver))
}

#[cfg(target_os = "macos")]
fn platform_loader() -> Option<(Library, ProcResolver)> {
    // The OpenGL framework exports GL entry points directly; there is no CGL proc-address call
    // that covers core functions, so `GlProcs::get` resolves them by plain symbol lookup.
    let lib = unsafe {
        Library::new("/System/Library/Frameworks/OpenGL.framework/Versions/Current/OpenGL")
    }
    .ok()?;
    Some((lib, noop_resolver))
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn noop_resolver(_name: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_loader() -> Option<(Library, ProcResolver)> {
    [
        ("libGL.so.1", "glXGetProcAddressARB"),
        ("libGL.so", "glXGetProcAddressARB"),
        ("libEGL.so.1", "eglGetProcAddress"),
        ("libEGL.so", "eglGetProcAddress"),
    ]
    .into_iter()
    .find_map(|(lib_name, sym)| {
        let lib = unsafe { Library::new(lib_name) }.ok()?;
        let mut symbol = sym.as_bytes().to_vec();
        symbol.push(0);
        let resolver = unsafe { lib.get::<ProcResolver>(&symbol) }
            .ok()
            .map(|s| *s)?;
        Some((lib, resolver))
    })
}

fn load() -> Result<GlProcs> {
    let mut attempts = Vec::new();

    let epoxy = [
        "libepoxy.so.0",
        "libepoxy.so",
        "libepoxy.0.dylib",
        "epoxy-0.dll",
    ]
    .into_iter()
    .find_map(|name| match unsafe { Library::new(name) } {
        Ok(lib) => {
            // Confirm this really is epoxy before trusting it.
            if unsafe { lib.get::<*mut c_void>(b"epoxy_glGetIntegerv\0") }.is_ok() {
                Some(lib)
            } else {
                attempts.push(format!("{name}: no epoxy_* symbols"));
                None
            }
        }
        Err(e) => {
            attempts.push(format!("{name}: {e}"));
            None
        }
    });

    let loader = platform_loader();

    if epoxy.is_none() && loader.is_none() {
        return Err(Error::Surface(format!(
            "no way to resolve GL entry points (tried libepoxy, GLX and EGL): {}",
            attempts.join("; ")
        )));
    }

    Ok(GlProcs { epoxy, loader })
}

pub fn procs() -> Result<&'static GlProcs> {
    match PROCS.get_or_init(load) {
        Ok(p) => Ok(p),
        Err(e) => Err(e.clone()),
    }
}

/// Read the currently bound draw framebuffer. GtkGLArea binds its own FBO before emitting
/// `render`, so this is how we discover the target to hand mpv.
///
/// # Safety
/// A GL context must be current on the calling thread.
pub unsafe fn current_framebuffer() -> Result<i32> {
    let procs = procs()?;
    let ptr = procs.get(c"glGetIntegerv");
    if ptr.is_null() {
        return Err(Error::Surface("could not resolve glGetIntegerv".into()));
    }
    let get_integerv: GlGetIntegerv = std::mem::transmute(ptr);
    let mut fbo: c_int = 0;
    get_integerv(GL_FRAMEBUFFER_BINDING, &mut fbo);
    Ok(fbo)
}

pub const GL_VERSION: c_uint = 0x1F02;
pub const GL_RENDERER: c_uint = 0x1F01;

type GlGetString = unsafe extern "C" fn(c_uint) -> *const c_char;

/// Read `GL_VERSION`/`GL_RENDERER`. Doubles as a liveness check on the current context: if this
/// returns `None`, entry points cannot be resolved and mpv would crash trying.
///
/// # Safety
/// A GL context must be current on the calling thread.
pub unsafe fn describe_context() -> Option<String> {
    let procs = procs().ok()?;
    let ptr = procs.get(c"glGetString");
    if ptr.is_null() {
        return None;
    }
    let get_string: GlGetString = std::mem::transmute(ptr);
    let read = |e: c_uint| {
        let p = get_string(e);
        if p.is_null() {
            "?".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    Some(format!("{} / {}", read(GL_VERSION), read(GL_RENDERER)))
}

/// `get_proc_address` implementation handed to mpv's render context.
///
/// # Safety
/// Called by mpv with a NUL-terminated symbol name, with the GL context current.
pub unsafe extern "C" fn mpv_get_proc_address(
    _ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    match procs() {
        Ok(p) => p.get(CStr::from_ptr(name)),
        Err(e) => {
            log::error!("GL entry points unavailable: {e}");
            std::ptr::null_mut()
        }
    }
}
