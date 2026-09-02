//! The mpv render context.
//!
//! `render.h` imposes rules that are easy to violate and expensive to debug:
//!
//! * every `mpv_render_*` call must happen on the thread where the GL context is current, and it
//!   must be the *same* context the render context was created with;
//! * only one `mpv_render_*` call may be in flight at a time;
//! * none of them may be called from inside the update callback.
//!
//! `RenderContext` is `!Send` and `!Sync`, so the compiler refuses to let it reach another
//! thread at all. That is the whole enforcement mechanism, and it is why this type is
//! deliberately *not* stored in the plugin's shared state — only `MpvCore` lives there. The
//! render context is owned by the surface's draw callback, which the toolkit only ever runs on
//! the UI thread.
//!
//! The update callback consequently does nothing but wake the UI thread; it never touches the
//! render context itself.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use super::core::MpvCore;
use super::ffi::*;
use super::loader::{self, MpvLib};
use crate::error::{Error, Result};

/// Boxed "a new frame may be ready" notification. Called from an arbitrary mpv thread, so it may
/// only schedule work — never render.
pub type UpdateCallback = Arc<dyn Fn() + Send + Sync>;

/// A live mpv render context, pinned to the thread that created it.
pub struct RenderContext {
    ctx: *mut MpvRenderContext,
    lib: &'static MpvLib,
    /// Kept alive for exactly as long as mpv might invoke it.
    _update: Arc<UpdateCallback>,
    /// Makes the type `!Send` and `!Sync`, which is what enforces render.h's threading rules.
    _not_send: PhantomData<*const ()>,
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // `ctx` is the Arc<UpdateCallback> we handed to mpv; it stays alive until after we clear the
    // callback in `Drop`, so this borrow is valid whenever mpv can reach it.
    let cb = &*(ctx as *const UpdateCallback);
    cb();
}

impl RenderContext {
    /// Create a render context against the GL context current on *this* thread.
    ///
    /// `get_proc_address` must resolve GL entry points for that context. `on_update` is invoked
    /// from arbitrary threads and must only schedule a redraw.
    ///
    /// # Safety
    /// A GL context must be current on the calling thread, and must remain the current context
    /// for every later call on the returned value.
    pub unsafe fn new(
        core: &MpvCore,
        get_proc_address: GetProcAddressFn,
        get_proc_address_ctx: *mut c_void,
        on_update: UpdateCallback,
        advanced_control: bool,
    ) -> Result<Self> {
        let lib = core.lib();

        let mut gl_init = MpvOpenGLInitParams {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx,
        };

        // ADVANCED_CONTROL lets mpv do direct rendering and tells it we will drive redraws
        // ourselves via mpv_render_context_update(). It also makes threading violations fatal
        // rather than silently degrading, which is what we want: bugs surface loudly.
        let mut advanced: i32 = i32::from(advanced_control);

        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut gl_init as *mut MpvOpenGLInitParams as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut i32 as *mut c_void,
            },
            MpvRenderParam::terminator(),
        ];

        let mut ctx: *mut MpvRenderContext = std::ptr::null_mut();
        let rc = (lib.render_context_create)(&mut ctx, core.raw_handle(), params.as_mut_ptr());
        loader::check(lib, rc).map_err(|e| Error::RenderInit(e.to_string()))?;
        if ctx.is_null() {
            return Err(Error::RenderInit(
                "mpv_render_context_create returned NULL".into(),
            ));
        }

        // Double-Arc so the pointer handed to C stays stable and we keep an owning handle.
        let update = Arc::new(on_update);
        (lib.render_context_set_update_callback)(
            ctx,
            Some(update_trampoline),
            Arc::as_ptr(&update) as *mut c_void,
        );

        Ok(RenderContext {
            ctx,
            lib,
            _update: update,
            _not_send: PhantomData,
        })
    }

    /// Returns true when mpv has a new frame for us. Cheap; safe to poll from the draw handler.
    pub fn needs_redraw(&self) -> bool {
        let flags = unsafe { (self.lib.render_context_update)(self.ctx) };
        flags & MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Draw the current frame into `fbo`.
    ///
    /// `flip_y` must be true for framebuffers whose origin is top-left (GtkGLArea's is), and
    /// false for a bottom-left origin. Getting this wrong renders the video upside down.
    pub fn render(&self, fbo: i32, width: i32, height: i32, flip_y: bool) -> Result<()> {
        let mut fbo_param = MpvOpenGLFbo {
            fbo,
            w: width,
            h: height,
            // 0 means "let mpv figure it out from the bound framebuffer".
            internal_format: 0,
        };
        let mut flip: i32 = i32::from(flip_y);
        // Never block the UI thread waiting for a frame's presentation time.
        let mut block: i32 = 0;

        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo_param as *mut MpvOpenGLFbo as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut i32 as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: &mut block as *mut i32 as *mut c_void,
            },
            MpvRenderParam::terminator(),
        ];

        let rc = unsafe { (self.lib.render_context_render)(self.ctx, params.as_mut_ptr()) };
        loader::check(self.lib, rc)
    }

    /// Tell mpv the frame just drawn has been presented. Improves its timing estimates.
    pub fn report_swap(&self) {
        unsafe { (self.lib.render_context_report_swap)(self.ctx) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            // Clear the callback first so mpv cannot invoke it while we are tearing down.
            (self.lib.render_context_set_update_callback)(self.ctx, None, std::ptr::null_mut());
            // Must happen before the core is destroyed; `MpvInstance` owns that ordering.
            (self.lib.render_context_free)(self.ctx);
        }
    }
}
