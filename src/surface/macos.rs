//! macOS surface: an `NSOpenGLContext`-backed `NSView` inserted below the `WKWebView`.
//!
//! **UNVERIFIED.** This backend has never been run. CI compiles it for both arm64 and x86_64, so
//! the architecture-specific dispatch below is at least type-checked on both, but nothing here
//! has been exercised on real hardware. Treat it as a starting point, not a supported platform.
//! See the platform table in the README.
//!
//! Two deliberate choices:
//!
//! * **Raw Objective-C runtime, no binding crates.** `objc2`/`cocoa` would be more ergonomic, but
//!   they drag in build scripts that compile Objective-C, which makes the crate impossible to
//!   even type-check from a non-Apple machine. Everything here is plain `extern "C"`.
//! * **A subview, not a child `NSWindow`.** A child window ordered behind the main one is the
//!   other common approach, but it desynchronises during resize, full-screen transitions and
//!   Spaces changes. A subview is laid out by the same window that owns the webview.
//!
//! The webview is made transparent by wry when the window is created with `"transparent": true`,
//! which on macOS additionally requires Tauri's `macos-private-api` feature — that has App Store
//! implications, and is the main reason this backend is less pleasant than the Linux one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::gl;
use super::VideoRect;
use crate::error::{Error, Result};
use crate::mpv::core::MpvCore;
use crate::mpv::render::{RenderContext, UpdateCallback};

// ---------------------------------------------------------------------------
// Objective-C runtime
// ---------------------------------------------------------------------------

type Id = *mut c_void;
type Sel = *mut c_void;

extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    /// x86_64 only. Apple's arm64 runtime does not export this symbol at all, so referencing it
    /// unconditionally would fail to link on Apple Silicon.
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

// The whole reason `msg_stret!` exists: 32 bytes is over the 16-byte threshold at which the
// System V ABI switches to returning through a hidden pointer.
const _: () = assert!(std::mem::size_of::<NSRect>() == 32);

fn class(name: &str) -> Id {
    let c = CString::new(name).expect("class name has no NUL");
    unsafe { objc_getClass(c.as_ptr()) }
}

fn sel(name: &str) -> Sel {
    let c = CString::new(name).expect("selector has no NUL");
    unsafe { sel_registerName(c.as_ptr()) }
}

/// `objc_msgSend` is variadic in the headers but must be called through a correctly typed
/// pointer; each helper below casts it to the exact signature of the message it sends.
///
/// **Do not use this for a return type larger than 16 bytes** — see `msg_stret!`. Struct
/// *arguments* passed by value are fine here on both architectures; it is only the return path
/// that differs.
macro_rules! msg {
    ($ret:ty $(, $arg:ty)* ; $obj:expr, $sel:expr $(, $a:expr)*) => {{
        let f: unsafe extern "C" fn(Id, Sel $(, $arg)*) -> $ret =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { f($obj, $sel $(, $a)*) }
    }};
}

/// Send a message that returns a struct too large to come back in registers (`NSRect`, 32 bytes).
///
/// The two architectures disagree, and getting it wrong is silent:
///
/// * **x86_64** — the System V ABI classes anything over 16 bytes as MEMORY, returning it through
///   a hidden pointer, and the Objective-C runtime requires `objc_msgSend_stret` for exactly that
///   case. Plain `objc_msgSend` does not implement the sret convention, so calling it through a
///   struct-returning signature reads back garbage geometry.
/// * **arm64** — an `NSRect` is a homogeneous floating-point aggregate returned in `v0`–`v3`, so
///   there is no sret and Apple ships no `objc_msgSend_stret`. Plain `objc_msgSend` is correct.
///
/// Note that CI's `macos-latest` runner is Apple Silicon and therefore only ever compiles the
/// second branch natively; the workflow also runs `cargo check --target x86_64-apple-darwin` on
/// that runner, which is what keeps the first branch honest.
macro_rules! msg_stret {
    ($ret:ty $(, $arg:ty)* ; $obj:expr, $sel:expr $(, $a:expr)*) => {{
        // Rust emits the sret calling convention for a large struct return, which is precisely
        // what objc_msgSend_stret expects: buffer pointer first, then self and _cmd.
        #[cfg(target_arch = "x86_64")]
        let send = objc_msgSend_stret as *const ();
        #[cfg(not(target_arch = "x86_64"))]
        let send = objc_msgSend as *const ();

        const _: () = assert!(
            std::mem::size_of::<$ret>() > 16,
            "msg_stret! is for large struct returns; use msg! for anything returned in registers",
        );

        let f: unsafe extern "C" fn(Id, Sel $(, $arg)*) -> $ret =
            unsafe { std::mem::transmute(send) };
        unsafe { f($obj, $sel $(, $a)*) }
    }};
}

/// `[[Class alloc] init...]` helpers.
fn alloc(cls: Id) -> Id {
    msg!(Id; cls, sel("alloc"))
}

// NSOpenGLPixelFormatAttribute values (NSOpenGL.h).
const NSOPENGL_PFA_DOUBLE_BUFFER: u32 = 5;
const NSOPENGL_PFA_COLOR_SIZE: u32 = 8;
const NSOPENGL_PFA_OPENGL_PROFILE: u32 = 99;
const NSOPENGL_PROFILE_VERSION_3_2_CORE: u32 = 0x3200;
/// `NSWindowBelow` from NSWindow.h.
const NS_WINDOW_BELOW: i64 = -1;

// ---------------------------------------------------------------------------
// libdispatch — used to re-arm the render tick without defining an ObjC class
// ---------------------------------------------------------------------------

extern "C" {
    fn dispatch_after_f(
        when: u64,
        queue: *mut c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_time(when: u64, delta: i64) -> u64;
    static _dispatch_main_q: c_void;
}

const DISPATCH_TIME_NOW: u64 = 0;
/// ~60 Hz.
const TICK_NANOS: i64 = 16_000_000;

fn main_queue() -> *mut c_void {
    unsafe { &_dispatch_main_q as *const c_void as *mut c_void }
}

// ---------------------------------------------------------------------------
// Surface registry
// ---------------------------------------------------------------------------

const OFFSCREEN: f64 = -8.0;

thread_local! {
    static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
}

struct Surface {
    view: Id,
    gl_context: Id,
    content_view: Id,
    render_ctx: Option<RenderContext>,
    frame_pending: Arc<AtomicBool>,
    flip_y: bool,
    frames: u64,
    /// Cleared on detach so an in-flight tick stops re-arming.
    alive: Arc<AtomicBool>,
}

/// Re-arming tick. `context` is a leaked `Box<String>` naming the surface.
extern "C" fn tick(context: *mut c_void) {
    let label = unsafe { &*(context as *const String) };

    let keep_going = SURFACES.with(|s| {
        let mut surfaces = s.borrow_mut();
        let Some(surface) = surfaces.get_mut(label) else {
            return false;
        };
        if !surface.alive.load(Ordering::Acquire) {
            return false;
        }
        let Some(ctx) = surface.render_ctx.as_ref() else {
            return true;
        };

        msg!((); surface.gl_context, sel("makeCurrentContext"));

        // Polling is mandatory under ADVANCED_CONTROL; see the Linux backend for the failure
        // mode when it is skipped.
        let woken = surface.frame_pending.swap(false, Ordering::Acquire);
        if !woken && !ctx.needs_redraw() {
            return true;
        }

        let bounds: NSRect = msg_stret!(NSRect; surface.view, sel("bounds"));
        let backing: NSRect =
            msg_stret!(NSRect, NSRect; surface.view, sel("convertRectToBacking:"), bounds);
        let (w, h) = (backing.size.width as i32, backing.size.height as i32);
        if w <= 0 || h <= 0 {
            return true;
        }

        // Render into the context's default framebuffer.
        match ctx.render(0, w, h, surface.flip_y) {
            Ok(()) => {
                msg!((); surface.gl_context, sel("flushBuffer"));
                ctx.report_swap();
                if surface.frames == 0 {
                    log::info!(
                        "first mpv frame rendered ({w}x{h}, flip_y={})",
                        surface.flip_y
                    );
                }
                surface.frames += 1;
            }
            Err(e) => log::error!("mpv_render_context_render failed: {e}"),
        }
        true
    });

    if keep_going {
        unsafe {
            let when = dispatch_time(DISPATCH_TIME_NOW, TICK_NANOS);
            dispatch_after_f(when, main_queue(), context, tick);
        }
    } else {
        // Reclaim the leaked label now that the loop has stopped.
        drop(unsafe { Box::from_raw(context as *mut String) });
    }
}

/// Build the surface for `label` under the window's content view. Main thread only.
pub fn attach(
    label: &str,
    ns_window: Id,
    core: Arc<MpvCore>,
    flip_y: bool,
    advanced_control: bool,
) -> Result<()> {
    if SURFACES.with(|s| s.borrow().contains_key(label)) {
        return Err(Error::AlreadyInitialized {
            label: label.to_string(),
        });
    }
    if ns_window.is_null() {
        return Err(Error::Surface("the window has no NSWindow".into()));
    }

    let content_view: Id = msg!(Id; ns_window, sel("contentView"));
    if content_view.is_null() {
        return Err(Error::Surface("the NSWindow has no content view".into()));
    }

    // A core profile is required: mpv's renderer needs GL 3.2+.
    let attrs: [u32; 7] = [
        NSOPENGL_PFA_DOUBLE_BUFFER,
        NSOPENGL_PFA_COLOR_SIZE,
        24,
        NSOPENGL_PFA_OPENGL_PROFILE,
        NSOPENGL_PROFILE_VERSION_3_2_CORE,
        0,
        0,
    ];
    let pixel_format = msg!(
        Id, *const u32;
        alloc(class("NSOpenGLPixelFormat")),
        sel("initWithAttributes:"),
        attrs.as_ptr()
    );
    if pixel_format.is_null() {
        return Err(Error::Surface(
            "no OpenGL 3.2 core pixel format is available".into(),
        ));
    }

    let frame = NSRect {
        origin: NSPoint {
            x: OFFSCREEN,
            y: OFFSCREEN,
        },
        size: NSSize {
            width: 1.0,
            height: 1.0,
        },
    };
    let view = msg!(Id, NSRect; alloc(class("NSView")), sel("initWithFrame:"), frame);
    if view.is_null() {
        return Err(Error::Surface("could not create the video NSView".into()));
    }
    msg!((), bool; view, sel("setWantsBestResolutionOpenGLSurface:"), true);

    let gl_context = msg!(
        Id, Id, Id;
        alloc(class("NSOpenGLContext")),
        sel("initWithFormat:shareContext:"),
        pixel_format,
        std::ptr::null_mut()
    );
    if gl_context.is_null() {
        return Err(Error::Surface("could not create an NSOpenGLContext".into()));
    }
    msg!((), Id; gl_context, sel("setView:"), view);
    msg!((); gl_context, sel("makeCurrentContext"));

    // Insert *below* the webview so the transparent page composites over the video.
    msg!(
        (), Id, i64, Id;
        content_view,
        sel("addSubview:positioned:relativeTo:"),
        view,
        NS_WINDOW_BELOW,
        std::ptr::null_mut()
    );

    match unsafe { gl::describe_context() } {
        Some(desc) => log::info!("GL context ready: {desc}"),
        None => {
            msg!((); view, sel("removeFromSuperview"));
            return Err(Error::Surface(
                "GL entry points could not be resolved against the NSOpenGLContext".into(),
            ));
        }
    }

    let frame_pending = Arc::new(AtomicBool::new(false));
    let render_ctx = {
        let flag = frame_pending.clone();
        let on_update: UpdateCallback = Arc::new(move || {
            flag.store(true, Ordering::Release);
        });
        unsafe {
            RenderContext::new(
                &core,
                gl::mpv_get_proc_address,
                std::ptr::null_mut(),
                on_update,
                advanced_control,
            )?
        }
    };

    let alive = Arc::new(AtomicBool::new(true));
    SURFACES.with(|s| {
        s.borrow_mut().insert(
            label.to_string(),
            Surface {
                view,
                gl_context,
                content_view,
                render_ctx: Some(render_ctx),
                frame_pending,
                flip_y,
                frames: 0,
                alive,
            },
        )
    });

    // Kick off the render loop. The label is leaked into the tick and reclaimed when it stops.
    let context = Box::into_raw(Box::new(label.to_string())) as *mut c_void;
    unsafe {
        let when = dispatch_time(DISPATCH_TIME_NOW, TICK_NANOS);
        dispatch_after_f(when, main_queue(), context, tick);
    }
    Ok(())
}

/// Move/resize the surface. The frontend reports top-left CSS pixels; AppKit views are
/// positioned from the bottom-left, so the y axis is flipped against the content view's height.
pub fn set_geometry(label: &str, rect: VideoRect) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;

        if rect.is_empty() {
            msg!((), bool; surface.view, sel("setHidden:"), true);
            return Ok(());
        }

        let content_bounds: NSRect = msg_stret!(NSRect; surface.content_view, sel("bounds"));
        let frame = NSRect {
            origin: NSPoint {
                x: rect.x,
                y: content_bounds.size.height - (rect.y + rect.height),
            },
            size: NSSize {
                width: rect.width,
                height: rect.height,
            },
        };
        msg!((), NSRect; surface.view, sel("setFrame:"), frame);
        msg!((), bool; surface.view, sel("setHidden:"), false);
        // The context must be told the view's geometry changed.
        msg!((); surface.gl_context, sel("update"));
        Ok(())
    })
}

pub fn set_visible(label: &str, visible: bool) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;
        msg!((), bool; surface.view, sel("setHidden:"), !visible);
        Ok(())
    })
}

/// Tear the surface down. Returns once the render context is gone, which is the precondition for
/// destroying the mpv core.
pub fn detach(label: &str) -> Result<()> {
    let surface = SURFACES.with(|s| s.borrow_mut().remove(label));
    let Some(mut surface) = surface else {
        return Ok(());
    };
    // Stop the tick before dropping anything it touches.
    surface.alive.store(false, Ordering::Release);

    msg!((); surface.gl_context, sel("makeCurrentContext"));
    // Must happen with the GL context current and before the core is destroyed.
    surface.render_ctx.take();

    msg!((); surface.view, sel("removeFromSuperview"));
    msg!((); class("NSOpenGLContext"), sel("clearCurrentContext"));
    Ok(())
}

pub fn exists(label: &str) -> bool {
    SURFACES.with(|s| s.borrow().contains_key(label))
}
