//! Linux surface: a `GtkGLArea` beneath the WebKit webview, inside a `GtkOverlay`.
//!
//! This is the platform both published mpv plugins fail on, and the reason is always the same:
//! they use mpv's `--wid` embedding, which needs an X11 XID. There is no XID under Wayland, and
//! none for a GTK widget with client-side decorations, so embedding silently does nothing.
//!
//! Rendering through the render API into a `GtkGLArea` needs no XID at all, so this works
//! identically on X11 and Wayland. GTK composites the transparent webview over the GL area for
//! us, which also means the *window* need not be transparent — no compositing WM required.
//!
//! Everything in this module runs on the GTK main thread. The surfaces live in a thread-local
//! registry rather than in the plugin's shared state precisely because GTK widgets and the mpv
//! render context are both thread-affine; callers reach them via `Window::run_on_main_thread`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk::gdk;
use gtk::prelude::*;
use webkit2gtk::WebViewExt;

use super::gl;
use super::VideoRect;
use crate::error::{Error, Result};
use crate::mpv::core::MpvCore;
use crate::mpv::render::{RenderContext, UpdateCallback};

/// Where the surface sits before the frontend reports a real rect. Far enough off that its 1x1
/// allocation is clipped away, close enough that GTK still realizes it.
const OFFSCREEN: i32 = -8;

thread_local! {
    static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
}

struct Surface {
    fixed: gtk::Fixed,
    gl_area: gtk::GLArea,
    overlay: gtk::Overlay,
    webview: webkit2gtk::WebView,
    host: gtk::Container,
    /// Owned by the draw callbacks; `!Send`, so it can only ever be touched here.
    render_ctx: Rc<RefCell<Option<RenderContext>>>,
    tick_id: Option<gtk::TickCallbackId>,
}

/// Depth-first search for the WebKit webview and the container holding it.
fn find_webview(root: &gtk::Container) -> Option<(webkit2gtk::WebView, gtk::Container)> {
    for child in root.children() {
        if let Ok(view) = child.clone().downcast::<webkit2gtk::WebView>() {
            return Some((view, root.clone()));
        }
        if let Ok(container) = child.downcast::<gtk::Container>() {
            if let Some(found) = find_webview(&container) {
                return Some(found);
            }
        }
    }
    None
}

/// Re-insert a widget the way its host expects, preserving expand/fill for a `GtkBox`.
fn add_to_host(host: &gtk::Container, widget: &impl IsA<gtk::Widget>) {
    if let Some(gtk_box) = host.dynamic_cast_ref::<gtk::Box>() {
        gtk_box.pack_start(widget, true, true, 0);
    } else {
        host.add(widget);
    }
}

/// Build the surface for `label` and slot it under the webview.
///
/// Must run on the GTK main thread.
pub fn attach(
    label: &str,
    vbox: &gtk::Box,
    core: Arc<MpvCore>,
    flip_y: bool,
    advanced_control: bool,
) -> Result<()> {
    let already = SURFACES.with(|s| s.borrow().contains_key(label));
    if already {
        return Err(Error::AlreadyInitialized {
            label: label.to_string(),
        });
    }

    let container: gtk::Container = vbox.clone().upcast();
    let (webview, host) = find_webview(&container).ok_or_else(|| {
        Error::Surface(
            "could not find the WebKitWebView inside the window's widget tree; \
             the Tauri version in use may lay its windows out differently than expected"
                .into(),
        )
    })?;

    // Let whatever is behind the page show through. Doing this ourselves means the app does not
    // need `"transparent": true` on the window, so no compositing window manager is required.
    webview.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));

    let gl_area = gtk::GLArea::new();
    gl_area.set_has_depth_buffer(false);
    gl_area.set_has_stencil_buffer(false);
    // We drive redraws from mpv's update callback rather than every frame-clock tick.
    gl_area.set_auto_render(false);
    gl_area.set_size_request(0, 0);

    let fixed = gtk::Fixed::new();
    fixed.put(&gl_area, OFFSCREEN, OFFSCREEN);

    let overlay = gtk::Overlay::new();
    overlay.add(&fixed);

    // Detach the webview from its current parent *before* handing it to the overlay: GTK refuses
    // to reparent a widget that still has one, and the failure is only a warning, leaving a
    // half-built tree that never realizes.
    host.remove(&webview);
    overlay.add_overlay(&webview);
    // The webview covers the whole window; the GL area sits behind it at the requested rect.
    webview.set_halign(gtk::Align::Fill);
    webview.set_valign(gtk::Align::Fill);

    add_to_host(&host, &overlay);

    let render_ctx: Rc<RefCell<Option<RenderContext>>> = Rc::new(RefCell::new(None));
    let frame_pending = Arc::new(AtomicBool::new(false));

    // --- realize: the GL context exists now, so build the mpv render context on top of it.
    {
        let render_ctx = render_ctx.clone();
        let frame_pending = frame_pending.clone();
        let core = core.clone();
        gl_area.connect_realize(move |area| {
            area.make_current();
            if let Some(err) = area.error() {
                log::error!("GtkGLArea failed to realize a GL context: {err}");
                return;
            }
            // Probe before handing the context to mpv: if entry points cannot be resolved,
            // mpv_render_context_create segfaults rather than returning an error.
            match unsafe { gl::describe_context() } {
                Some(desc) => log::info!("GL context ready: {desc}"),
                None => {
                    log::error!(
                        "GL entry points could not be resolved against the GtkGLArea context; \
                         refusing to create the mpv render context"
                    );
                    return;
                }
            }
            let flag = frame_pending.clone();
            let on_update: UpdateCallback = Arc::new(move || {
                // Called from an arbitrary mpv thread: only ever set a flag. The tick callback
                // on the UI thread turns this into a queue_render().
                flag.store(true, Ordering::Release);
            });
            match unsafe {
                RenderContext::new(
                    &core,
                    gl::mpv_get_proc_address,
                    std::ptr::null_mut(),
                    on_update,
                    advanced_control,
                )
            } {
                Ok(ctx) => {
                    log::info!("mpv render context created for the GtkGLArea");
                    *render_ctx.borrow_mut() = Some(ctx);
                }
                Err(e) => log::error!("failed to create the mpv render context: {e}"),
            }
        });
    }

    // --- render: GtkGLArea has made the context current and bound its FBO.
    {
        let render_ctx = render_ctx.clone();
        let frames = std::cell::Cell::new(0u64);
        gl_area.connect_render(move |area, _ctx| {
            if let Some(ctx) = render_ctx.borrow().as_ref() {
                let scale = area.scale_factor();
                let w = area.allocated_width() * scale;
                let h = area.allocated_height() * scale;
                if w > 0 && h > 0 {
                    match unsafe { gl::current_framebuffer() } {
                        Ok(fbo) => {
                            match ctx.render(fbo, w, h, flip_y) {
                                Ok(()) => {
                                    let n = frames.get();
                                    frames.set(n + 1);
                                    if n == 0 {
                                        log::info!(
                                            "first mpv frame rendered ({w}x{h}, fbo {fbo}, flip_y={flip_y})"
                                        );
                                    } else if n % 300 == 0 {
                                        log::debug!("rendered {n} mpv frames");
                                    }
                                }
                                Err(e) => log::error!("mpv_render_context_render failed: {e}"),
                            }
                            ctx.report_swap();
                        }
                        Err(e) => log::error!("could not read the bound framebuffer: {e}"),
                    }
                }
            }
            glib::Propagation::Stop
        });
    }

    // --- unrealize: tear the render context down while its GL context is still current.
    {
        let render_ctx = render_ctx.clone();
        gl_area.connect_unrealize(move |area| {
            area.make_current();
            // Dropping calls mpv_render_context_free, which must happen before the core dies.
            render_ctx.borrow_mut().take();
        });
    }

    // --- tick: the only thing that converts "mpv has a frame" into an actual redraw.
    //
    // Asking mpv via `mpv_render_context_update()` is not optional. Under
    // MPV_RENDER_PARAM_ADVANCED_CONTROL the update callback is only a wake-up hint; mpv expects
    // the client to poll for readiness, and if you never poll, the pipeline stalls after the
    // first frame and every later frame renders black.
    let tick_id = {
        let render_ctx = render_ctx.clone();
        let frame_pending = frame_pending.clone();
        Some(gl_area.add_tick_callback(move |area, _clock| {
            let woken = frame_pending.swap(false, Ordering::Acquire);
            let has_frame = match render_ctx.borrow().as_ref() {
                Some(ctx) => {
                    // Keep render.h's contract: the GL context must be current for any
                    // mpv_render_* call, including this one.
                    area.make_current();
                    ctx.needs_redraw()
                }
                None => false,
            };
            if woken || has_frame {
                area.queue_render();
            }
            glib::ControlFlow::Continue
        }))
    };

    // Park the surface offscreen at 1x1 rather than hiding it: GTK never realizes a hidden
    // widget, so hiding here would mean the GL context — and with it the mpv render context —
    // is never created. It gets moved into place by the first set_geometry call.
    fixed.move_(&gl_area, OFFSCREEN, OFFSCREEN);
    gl_area.set_size_request(1, 1);
    overlay.show_all();

    SURFACES.with(|s| {
        s.borrow_mut().insert(
            label.to_string(),
            Surface {
                fixed,
                gl_area,
                overlay,
                webview,
                host,
                render_ctx,
                tick_id,
            },
        )
    });

    Ok(())
}

/// Move/resize the video surface to match the frontend element. Main thread only.
pub fn set_geometry(label: &str, rect: VideoRect) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;

        let (x, y, w, h) = rect.normalized();
        if rect.is_empty() {
            // Park rather than hide, for the same realize reason as in `attach`.
            surface.fixed.move_(&surface.gl_area, OFFSCREEN, OFFSCREEN);
            surface.gl_area.set_size_request(1, 1);
            return Ok(());
        }
        surface.fixed.move_(&surface.gl_area, x, y);
        surface.gl_area.set_size_request(w, h);
        surface.gl_area.show();
        Ok(())
    })
}

/// Show or hide the surface without forgetting its geometry. Main thread only.
pub fn set_visible(label: &str, visible: bool) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;
        if visible {
            surface.gl_area.show();
        } else {
            surface.fixed.move_(&surface.gl_area, OFFSCREEN, OFFSCREEN);
            surface.gl_area.set_size_request(1, 1);
        }
        Ok(())
    })
}

/// Tear the surface down and put the webview back where Tauri left it. Main thread only.
///
/// Returns once the render context is gone, which is the precondition for destroying the core.
pub fn detach(label: &str) -> Result<()> {
    let surface = SURFACES.with(|s| s.borrow_mut().remove(label));
    let Some(surface) = surface else {
        return Ok(());
    };

    if let Some(tick) = surface.tick_id {
        tick.remove();
    }

    // Free the render context explicitly rather than waiting for unrealize, so the caller can
    // destroy the mpv core immediately afterwards.
    surface.gl_area.make_current();
    surface.render_ctx.borrow_mut().take();

    surface.fixed.remove(&surface.gl_area);
    surface.overlay.remove(&surface.webview);
    surface.host.remove(&surface.overlay);
    add_to_host(&surface.host, &surface.webview);
    surface.webview.show();

    Ok(())
}

/// True when a surface exists for this window. Main thread only.
pub fn exists(label: &str) -> bool {
    SURFACES.with(|s| s.borrow().contains_key(label))
}
