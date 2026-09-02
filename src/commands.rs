//! Tauri command handlers.
//!
//! Anything touching the GL surface has to reach the UI thread; `on_main_thread` handles that,
//! including the case where the caller is already on it (blocking on `run_on_main_thread` from
//! the main thread would deadlock).

use std::sync::Arc;

use serde_json::Value;
use tauri::{Emitter, Runtime, State, WebviewWindow};

use crate::error::{Error, Result};
use crate::mpv::core::{MpvCore, MpvEventPayload};
use crate::state::{MpvConfig, MpvInstance, MpvState};
use crate::surface::VideoRect;
use crate::EVENT_NAME;

/// Run `f` on the UI thread and wait for its result.
fn on_main_thread<R: Runtime, T: Send + 'static>(
    window: &WebviewWindow<R>,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    // Already on the UI thread: calling run_on_main_thread here would queue the work behind
    // ourselves and block forever.
    #[cfg(target_os = "linux")]
    {
        if gtk::is_initialized_main_thread() {
            return f();
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    window.run_on_main_thread(move || {
        let _ = tx.send(f());
    })?;
    rx.recv()
        .map_err(|e| Error::Surface(format!("main-thread call did not complete: {e}")))?
}

/// Platform default for the framebuffer's vertical orientation.
///
/// Every backend here draws into a framebuffer using OpenGL's own convention (origin at the
/// bottom-left), which is what mpv assumes when `FLIP_Y` is unset — so the default is `false`
/// everywhere.
///
/// This is reasoned from the toolkits' documented behaviour, **not** confirmed on screen: the
/// only GL stack available during development was llvmpipe, where mpv's renderer emits
/// near-black output (reproducible with stock mpv — see `tests/e2e/README.md`), so orientation
/// could not be observed. It is the classic thing to get wrong in a render-API integration,
/// which is exactly why `MpvConfig::flip_y` exists: if video appears upside down, set it.
fn default_flip_y() -> bool {
    false
}

#[tauri::command]
pub async fn init<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    config: Option<MpvConfig>,
) -> Result<()> {
    let label = window.label().to_string();
    if state.contains(&label) {
        return Err(Error::AlreadyInitialized { label });
    }

    let config = config.unwrap_or_default();
    let log_level = config
        .log_level
        .clone()
        .unwrap_or_else(|| "warn".to_string());
    let core = Arc::new(MpvCore::new(&config.options, &log_level)?);

    // Forward mpv events to this window's frontend.
    let sink = {
        let window = window.clone();
        Arc::new(move |payload: MpvEventPayload| {
            if let Err(e) = window.emit(EVENT_NAME, &payload) {
                log::warn!("failed to emit mpv event: {e}");
            }
        })
    };
    let pump = core.spawn_event_thread(sink);
    let instance = Arc::new(MpvInstance::new(core.clone(), pump));

    // Build the surface on the UI thread.
    let flip_y = config.flip_y.unwrap_or_else(default_flip_y);
    let advanced = config.advanced_control.unwrap_or(false);
    let attach_result = {
        let window_for_main = window.clone();
        let label = label.clone();
        let core = core.clone();
        on_main_thread(&window, move || -> Result<()> {
            #[cfg(target_os = "linux")]
            {
                let vbox = window_for_main.default_vbox()?;
                crate::surface::linux::attach(&label, &vbox, core, flip_y, advanced)
            }
            #[cfg(windows)]
            {
                // Tauri hands back the `windows` crate's HWND newtype; this backend is built on
                // `windows-sys`, where an HWND is a plain pointer.
                let hwnd = window_for_main.hwnd()?;
                let hwnd = hwnd.0 as *mut std::ffi::c_void;
                crate::surface::windows::attach(&label, hwnd, core, flip_y, advanced)
            }
            #[cfg(target_os = "macos")]
            {
                let ns_window = window_for_main.ns_window()?;
                crate::surface::macos::attach(&label, ns_window, core, flip_y, advanced)
            }
            #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
            {
                let _ = (window_for_main, label, core, flip_y, advanced);
                Err(Error::UnsupportedPlatform)
            }
        })
    };

    if let Err(e) = attach_result {
        // Surface creation failed: tear the core down rather than leaving a half-live instance.
        instance.shutdown();
        return Err(e);
    }

    for name in &config.observe {
        instance.observe(name)?;
    }

    state.insert(label, instance);
    Ok(())
}

#[tauri::command]
pub async fn destroy<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
) -> Result<()> {
    let label = window.label().to_string();
    let Some(instance) = state.remove(&label) else {
        return Ok(());
    };

    // Order matters: the render context must be freed before the core is destroyed. `detach`
    // does the first, `shutdown` (and then dropping the last Arc) does the second.
    let detach_label = label.clone();
    on_main_thread(&window, move || -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            crate::surface::linux::detach(&detach_label)
        }
        #[cfg(windows)]
        {
            crate::surface::windows::detach(&detach_label)
        }
        #[cfg(target_os = "macos")]
        {
            crate::surface::macos::detach(&detach_label)
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            let _ = detach_label;
            Ok(())
        }
    })?;

    instance.shutdown();
    Ok(())
}

#[tauri::command]
pub async fn command<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    args: Value,
) -> Result<Value> {
    state.get(window.label())?.core.command(&args)
}

#[tauri::command]
pub async fn set_property<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    name: String,
    value: Value,
) -> Result<()> {
    state.get(window.label())?.core.set_property(&name, &value)
}

#[tauri::command]
pub async fn get_property<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    name: String,
) -> Result<Value> {
    state.get(window.label())?.core.get_property(&name)
}

#[tauri::command]
pub async fn observe_property<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    name: String,
) -> Result<()> {
    state.get(window.label())?.observe(&name)
}

#[tauri::command]
pub async fn unobserve_property<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    name: String,
) -> Result<()> {
    state.get(window.label())?.unobserve(&name)
}

#[tauri::command]
pub async fn set_video_rect<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    rect: VideoRect,
) -> Result<()> {
    let label = window.label().to_string();
    // Confirm the instance exists before marshalling to the UI thread.
    state.get(&label)?;
    // Child-window coordinates are physical pixels on Windows; the frontend reports CSS pixels.
    let scale = window.scale_factor().unwrap_or(1.0);
    on_main_thread(&window, move || -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            // GTK widget coordinates are already logical, so the scale factor is not applied.
            let _ = scale;
            crate::surface::linux::set_geometry(&label, rect)
        }
        #[cfg(windows)]
        {
            crate::surface::windows::set_geometry(&label, rect, scale)
        }
        #[cfg(target_os = "macos")]
        {
            // AppKit view coordinates are logical points, so the scale factor is not applied.
            let _ = scale;
            crate::surface::macos::set_geometry(&label, rect)
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            let _ = (label, rect, scale);
            Ok(())
        }
    })
}

#[tauri::command]
pub async fn set_surface_visible<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, MpvState>,
    visible: bool,
) -> Result<()> {
    let label = window.label().to_string();
    state.get(&label)?;
    on_main_thread(&window, move || -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            crate::surface::linux::set_visible(&label, visible)
        }
        #[cfg(windows)]
        {
            crate::surface::windows::set_visible(&label, visible)
        }
        #[cfg(target_os = "macos")]
        {
            crate::surface::macos::set_visible(&label, visible)
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            let _ = (label, visible);
            Ok(())
        }
    })
}
