//! # tauri-plugin-mpv-surface
//!
//! Composites a real mpv video surface *beneath* a transparent Tauri webview, so your HTML UI
//! draws over live video with alpha.
//!
//! Unlike the existing mpv plugins, this one does not use mpv's `--wid` window embedding — mpv's
//! own maintainers warn it has "various platform-specific behavior and problems (in particular on
//! OSX)", and on Linux it needs an X11 XID that does not exist under Wayland. Instead the video
//! goes through libmpv's **render API** into a GPU surface this plugin owns and positions behind
//! the webview, which works the same way on every platform.
//!
//! libmpv is loaded at *runtime*, so building this crate needs no native mpv, no pkg-config, and
//! no bindgen.

pub mod error;
pub mod mpv;
pub mod surface;

mod commands;
mod state;

pub use error::{Error, Result};
pub use state::MpvConfig;
pub use surface::VideoRect;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

/// Event name the plugin emits mpv events under.
pub const EVENT_NAME: &str = "mpv-surface:event";

/// Initialize the plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("mpv-surface")
        .invoke_handler(tauri::generate_handler![
            commands::init,
            commands::destroy,
            commands::command,
            commands::set_property,
            commands::get_property,
            commands::observe_property,
            commands::unobserve_property,
            commands::set_video_rect,
            commands::set_surface_visible,
        ])
        .setup(|app, _api| {
            app.manage(state::MpvState::default());
            Ok(())
        })
        .on_window_ready(|_window| {})
        .build()
}
