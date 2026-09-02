//! Windows surface: a child `HWND` with a WGL context, z-ordered beneath the WebView2 control.
//!
//! The webview is made transparent by wry when the window is created with `"transparent": true`,
//! so — unlike the Linux backend — that setting *is* required here. Doing it ourselves would mean
//! driving `ICoreWebView2Controller2::put_DefaultBackgroundColor` over COM for a result Tauri
//! already offers.
//!
//! Redraws are driven by a timer rather than an event: `mpv_render_context_update()` has to be
//! polled anyway (see the Linux backend for why), and a timer that polls is simpler and less
//! racy than posting a message from mpv's update callback.
//!
//! **UNVERIFIED**: compile-checked for `x86_64-pc-windows-gnu` and in CI on a Windows runner, but
//! never run on real hardware. See the platform-support table in the README.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC, HBRUSH, HDC};
use windows_sys::Win32::Graphics::OpenGL::{
    wglCreateContext, wglDeleteContext, wglMakeCurrent, ChoosePixelFormat, SetPixelFormat,
    SwapBuffers, HGLRC, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_MAIN_PLANE, PFD_SUPPORT_OPENGL,
    PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, KillTimer, RegisterClassW, SetTimer,
    SetWindowPos, ShowWindow, HWND_BOTTOM, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA, WNDCLASSW, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};

use super::gl;
use super::VideoRect;
use crate::error::{Error, Result};
use crate::mpv::core::MpvCore;
use crate::mpv::render::{RenderContext, UpdateCallback};

/// Where the surface sits before the frontend reports a real rect.
const OFFSCREEN: i32 = -8;
/// ~60 Hz. The timer only polls mpv; it does not force a redraw.
const TIMER_INTERVAL_MS: u32 = 16;
const TIMER_ID: usize = 1;

thread_local! {
    static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
    /// Reverse lookup for the timer callback, which only receives an HWND.
    static BY_HWND: RefCell<HashMap<isize, String>> = RefCell::new(HashMap::new());
}

struct Surface {
    hwnd: HWND,
    hdc: HDC,
    hglrc: HGLRC,
    render_ctx: Option<RenderContext>,
    frame_pending: Arc<AtomicBool>,
    flip_y: bool,
    frames: u64,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Registers the child-window class once per process.
fn ensure_class() -> Result<Vec<u16>> {
    thread_local! {
        static REGISTERED: RefCell<bool> = const { RefCell::new(false) };
    }
    let name = wide("TauriPluginMpvSurface");
    REGISTERED.with(|r| {
        let mut registered = r.borrow_mut();
        if *registered {
            return Ok(());
        }
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(Error::Surface(format!(
                "RegisterClassW failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        *registered = true;
        Ok(())
    })?;
    Ok(name)
}

/// Create the WGL context for `hwnd` and make it current.
unsafe fn create_gl_context(hwnd: HWND) -> Result<(HDC, HGLRC)> {
    let hdc = GetDC(hwnd);
    if hdc.is_null() {
        return Err(Error::Surface("GetDC returned NULL".into()));
    }

    let mut pfd: PIXELFORMATDESCRIPTOR = std::mem::zeroed();
    pfd.nSize = std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16;
    pfd.nVersion = 1;
    pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
    pfd.iPixelType = PFD_TYPE_RGBA;
    pfd.cColorBits = 32;
    pfd.cDepthBits = 0;
    pfd.cStencilBits = 0;
    pfd.iLayerType = PFD_MAIN_PLANE as u8;

    let format = ChoosePixelFormat(hdc, &pfd);
    if format == 0 {
        ReleaseDC(hwnd, hdc);
        return Err(Error::Surface(
            "ChoosePixelFormat found no usable format".into(),
        ));
    }
    if SetPixelFormat(hdc, format, &pfd) == 0 {
        ReleaseDC(hwnd, hdc);
        return Err(Error::Surface("SetPixelFormat failed".into()));
    }

    let hglrc = wglCreateContext(hdc);
    if hglrc.is_null() {
        ReleaseDC(hwnd, hdc);
        return Err(Error::Surface("wglCreateContext failed".into()));
    }
    if wglMakeCurrent(hdc, hglrc) == 0 {
        wglDeleteContext(hglrc);
        ReleaseDC(hwnd, hdc);
        return Err(Error::Surface("wglMakeCurrent failed".into()));
    }
    Ok((hdc, hglrc))
}

unsafe extern "system" fn timer_proc(hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    let label = BY_HWND.with(|m| m.borrow().get(&(hwnd as isize)).cloned());
    let Some(label) = label else { return };

    SURFACES.with(|s| {
        let mut surfaces = s.borrow_mut();
        let Some(surface) = surfaces.get_mut(&label) else {
            return;
        };
        let Some(ctx) = surface.render_ctx.as_ref() else {
            return;
        };

        if wglMakeCurrent(surface.hdc, surface.hglrc) == 0 {
            return;
        }
        // Polling is mandatory under ADVANCED_CONTROL; see the Linux backend for the failure
        // mode when it is skipped.
        let woken = surface.frame_pending.swap(false, Ordering::Acquire);
        if !woken && !ctx.needs_redraw() {
            return;
        }

        let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect);
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return;
        }

        // Render to the window's default framebuffer.
        match ctx.render(0, w, h, surface.flip_y) {
            Ok(()) => {
                SwapBuffers(surface.hdc);
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
    });
}

/// Build the surface for `label` under the Tauri window's HWND. Main thread only.
pub fn attach(
    label: &str,
    parent: HWND,
    core: Arc<MpvCore>,
    flip_y: bool,
    advanced_control: bool,
) -> Result<()> {
    if SURFACES.with(|s| s.borrow().contains_key(label)) {
        return Err(Error::AlreadyInitialized {
            label: label.to_string(),
        });
    }

    let class = ensure_class()?;
    let title = wide("");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            OFFSCREEN,
            OFFSCREEN,
            1,
            1,
            parent,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(Error::Surface(format!(
            "CreateWindowExW failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    // Sit beneath the WebView2 control so the transparent page composites over the video.
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_BOTTOM,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | 0x0001 | 0x0002,
        );
    }

    let (hdc, hglrc) = unsafe { create_gl_context(hwnd)? };

    if let Some(desc) = unsafe { gl::describe_context() } {
        log::info!("GL context ready: {desc}");
    } else {
        unsafe {
            wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut());
            wglDeleteContext(hglrc);
            ReleaseDC(hwnd, hdc);
            DestroyWindow(hwnd);
        }
        return Err(Error::Surface(
            "GL entry points could not be resolved against the WGL context".into(),
        ));
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

    BY_HWND.with(|m| m.borrow_mut().insert(hwnd as isize, label.to_string()));
    SURFACES.with(|s| {
        s.borrow_mut().insert(
            label.to_string(),
            Surface {
                hwnd,
                hdc,
                hglrc,
                render_ctx: Some(render_ctx),
                frame_pending,
                flip_y,
                frames: 0,
            },
        )
    });

    unsafe { SetTimer(hwnd, TIMER_ID, TIMER_INTERVAL_MS, Some(timer_proc)) };
    Ok(())
}

/// Move/resize the surface. `scale` converts the frontend's CSS pixels to physical pixels,
/// which is what child-window coordinates are measured in.
pub fn set_geometry(label: &str, rect: VideoRect, scale: f64) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;

        let scaled = VideoRect {
            x: rect.x * scale,
            y: rect.y * scale,
            width: rect.width * scale,
            height: rect.height * scale,
        };
        let (x, y, w, h) = scaled.normalized();
        unsafe {
            if scaled.is_empty() {
                ShowWindow(surface.hwnd, SW_HIDE);
            } else {
                SetWindowPos(surface.hwnd, HWND_BOTTOM, x, y, w, h, SWP_NOACTIVATE);
                ShowWindow(surface.hwnd, SW_SHOWNA);
            }
        }
        Ok(())
    })
}

pub fn set_visible(label: &str, visible: bool) -> Result<()> {
    SURFACES.with(|s| {
        let surfaces = s.borrow();
        let surface = surfaces.get(label).ok_or_else(|| Error::NotInitialized {
            label: label.to_string(),
        })?;
        unsafe { ShowWindow(surface.hwnd, if visible { SW_SHOWNA } else { SW_HIDE }) };
        Ok(())
    })
}

/// Tear the surface down. Returns once the render context is gone, which is the precondition
/// for destroying the mpv core.
pub fn detach(label: &str) -> Result<()> {
    let surface = SURFACES.with(|s| s.borrow_mut().remove(label));
    let Some(mut surface) = surface else {
        return Ok(());
    };
    BY_HWND.with(|m| m.borrow_mut().remove(&(surface.hwnd as isize)));

    unsafe {
        KillTimer(surface.hwnd, TIMER_ID);
        // The render context must be freed with its GL context current, before the core dies.
        wglMakeCurrent(surface.hdc, surface.hglrc);
        surface.render_ctx.take();
        wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut());
        wglDeleteContext(surface.hglrc);
        ReleaseDC(surface.hwnd, surface.hdc);
        DestroyWindow(surface.hwnd);
    }
    Ok(())
}

pub fn exists(label: &str) -> bool {
    SURFACES.with(|s| s.borrow().contains_key(label))
}

/// Unused on this platform; kept so the module surface matches the other backends.
#[allow(dead_code)]
fn _unused(_: COLORREF, _: *mut c_void) {}
