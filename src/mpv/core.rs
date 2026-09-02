//! The mpv core handle: creation, commands, properties, and the event pump.
//!
//! Everything here is safe to call from any thread — libmpv's client API is thread-safe. The
//! *render* half is not, and lives in `render.rs` behind types that enforce its rules.

use std::collections::BTreeMap;
use std::ffi::{c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::ffi::*;
use super::loader::{self, MpvLib};
use super::node::{json_to_node, node_to_json, MpvOwnedNode};
use crate::error::{Error, Result};

/// An event from mpv, in the shape the frontend receives it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum MpvEventPayload {
    PropertyChange {
        name: String,
        value: Value,
    },
    StartFile,
    FileLoaded,
    EndFile {
        reason: String,
    },
    Seek,
    PlaybackRestart,
    Shutdown,
    LogMessage {
        prefix: String,
        level: String,
        text: String,
    },
}

pub type EventSink = Arc<dyn Fn(MpvEventPayload) + Send + Sync>;

/// A live mpv instance.
pub struct MpvCore {
    handle: *mut MpvHandle,
    lib: &'static MpvLib,
    shutting_down: Arc<AtomicBool>,
}

// libmpv's client API is explicitly thread-safe; `handle` may be used from any thread.
unsafe impl Send for MpvCore {}
unsafe impl Sync for MpvCore {}

impl MpvCore {
    /// Create and initialize an mpv instance.
    ///
    /// `options` are applied *before* `mpv_initialize`, which is required for the ones that can
    /// only be set at that point (`vo`, `hwdec`, ...). Caller-supplied entries override the
    /// defaults, except `vo`, which must stay `libmpv` for the render API to work at all.
    pub fn new(options: &BTreeMap<String, String>, log_level: &str) -> Result<Self> {
        let lib = loader::get()?;

        let handle = unsafe { (lib.create)() };
        if handle.is_null() {
            return Err(Error::RenderInit("mpv_create returned NULL".into()));
        }

        let core = MpvCore {
            handle,
            lib,
            shutting_down: Arc::new(AtomicBool::new(false)),
        };

        // Defaults chosen to match what an embedding host wants; all overridable except `vo`.
        let mut opts: BTreeMap<&str, String> = BTreeMap::new();
        opts.insert("hwdec", "auto-safe".to_string());
        // Without this, mpv would try to open its own window.
        opts.insert("force-window", "no".to_string());
        opts.insert("audio-display", "no".to_string());
        for (k, v) in options {
            if k == "vo" {
                log::warn!("ignoring `vo` option {v:?}: the render API requires vo=libmpv");
                continue;
            }
            opts.insert(k.as_str(), v.clone());
        }
        // Set last so nothing can override it.
        opts.insert("vo", "libmpv".to_string());

        for (k, v) in &opts {
            core.set_option(k, v)?;
        }

        loader::check(lib, unsafe { (lib.initialize)(handle) })?;

        // Surface mpv's own diagnostics. Without these, a failed `loadfile` is invisible: the
        // render pipeline keeps happily drawing black frames and nothing says why.
        let level = CString::new(log_level).unwrap_or_else(|_| CString::new("warn").unwrap());
        let rc = unsafe { (lib.request_log_messages)(handle, level.as_ptr()) };
        if let Err(e) = loader::check(lib, rc) {
            log::warn!("could not enable mpv log messages: {e}");
        }

        Ok(core)
    }

    fn set_option(&self, name: &str, value: &str) -> Result<()> {
        let c_name = CString::new(name)
            .map_err(|_| Error::InvalidArgument(format!("option name has a NUL: {name:?}")))?;
        let c_value = CString::new(value)
            .map_err(|_| Error::InvalidArgument(format!("option value has a NUL: {value:?}")))?;
        let rc =
            unsafe { (self.lib.set_option_string)(self.handle, c_name.as_ptr(), c_value.as_ptr()) };
        loader::check(self.lib, rc).map_err(|e| match e {
            Error::Mpv { code, message } => Error::Mpv {
                code,
                message: format!("setting option `{name}` to {value:?}: {message}"),
            },
            other => other,
        })
    }

    pub(crate) fn raw_handle(&self) -> *mut MpvHandle {
        self.handle
    }

    pub(crate) fn lib(&self) -> &'static MpvLib {
        self.lib
    }

    /// Run an mpv command. `args` is the full argument array, e.g. `["loadfile", url]`.
    pub fn command(&self, args: &Value) -> Result<Value> {
        if !args.is_array() {
            return Err(Error::InvalidArgument(
                "command arguments must be an array, e.g. [\"loadfile\", \"<url>\"]".into(),
            ));
        }
        let mut owned = json_to_node(args)?;
        let mut result = MpvOwnedNode::empty();
        let rc =
            unsafe { (self.lib.command_node)(self.handle, owned.as_mut_ptr(), &mut result.node) };
        loader::check(self.lib, rc)?;
        Ok(result.to_json())
    }

    pub fn set_property(&self, name: &str, value: &Value) -> Result<()> {
        let c_name = CString::new(name)
            .map_err(|_| Error::InvalidArgument(format!("property name has a NUL: {name:?}")))?;
        let mut owned = json_to_node(value)?;
        let rc = unsafe {
            (self.lib.set_property)(
                self.handle,
                c_name.as_ptr(),
                MPV_FORMAT_NODE,
                owned.as_mut_ptr() as *mut c_void,
            )
        };
        loader::check(self.lib, rc)
    }

    /// Read a property as JSON. Returns `Ok(Value::Null)` when the property exists but currently
    /// has no value (e.g. `time-pos` before playback starts), which mpv reports as a distinct
    /// error code rather than a value.
    pub fn get_property(&self, name: &str) -> Result<Value> {
        let c_name = CString::new(name)
            .map_err(|_| Error::InvalidArgument(format!("property name has a NUL: {name:?}")))?;
        let mut result = MpvOwnedNode::empty();
        let rc = unsafe {
            (self.lib.get_property)(
                self.handle,
                c_name.as_ptr(),
                MPV_FORMAT_NODE,
                &mut result.node as *mut MpvNode as *mut c_void,
            )
        };
        if rc == MPV_ERROR_PROPERTY_UNAVAILABLE {
            return Ok(Value::Null);
        }
        loader::check(self.lib, rc)?;
        Ok(result.to_json())
    }

    pub fn observe_property(&self, id: u64, name: &str) -> Result<()> {
        let c_name = CString::new(name)
            .map_err(|_| Error::InvalidArgument(format!("property name has a NUL: {name:?}")))?;
        let rc = unsafe {
            (self.lib.observe_property)(self.handle, id, c_name.as_ptr(), MPV_FORMAT_NODE)
        };
        loader::check(self.lib, rc)
    }

    pub fn unobserve_property(&self, id: u64) -> Result<()> {
        let rc = unsafe { (self.lib.unobserve_property)(self.handle, id) };
        // Returns the number of properties unobserved, which is a success value, not an error.
        if rc >= 0 {
            Ok(())
        } else {
            loader::check(self.lib, rc)
        }
    }

    /// Spawn the event pump. The returned handle must be joined before the core is destroyed.
    pub fn spawn_event_thread(&self, sink: EventSink) -> EventPump {
        let handle = SendHandle(self.handle);
        let lib = self.lib;
        let shutting_down = self.shutting_down.clone();

        let join = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || {
                let handle = handle;
                loop {
                    // Blocks until an event arrives; woken by mpv_wakeup on shutdown.
                    let ev = unsafe { (lib.wait_event)(handle.0, -1.0) };
                    if ev.is_null() {
                        continue;
                    }
                    let ev = unsafe { &*ev };

                    match ev.event_id {
                        MPV_EVENT_NONE => {}
                        MPV_EVENT_SHUTDOWN => {
                            sink(MpvEventPayload::Shutdown);
                            break;
                        }
                        MPV_EVENT_PROPERTY_CHANGE => {
                            let prop = ev.data as *const MpvEventProperty;
                            if prop.is_null() {
                                continue;
                            }
                            let prop = unsafe { &*prop };
                            if prop.name.is_null() {
                                continue;
                            }
                            let name = unsafe { CStr::from_ptr(prop.name) }
                                .to_string_lossy()
                                .into_owned();
                            let value = if prop.format == MPV_FORMAT_NONE || prop.data.is_null() {
                                Value::Null
                            } else {
                                unsafe { node_to_json(prop.data as *const MpvNode) }
                            };
                            sink(MpvEventPayload::PropertyChange { name, value });
                        }
                        MPV_EVENT_LOG_MESSAGE => {
                            if ev.data.is_null() {
                                continue;
                            }
                            let msg = unsafe { &*(ev.data as *const MpvEventLogMessage) };
                            let cstr = |p: *const std::ffi::c_char| {
                                if p.is_null() {
                                    String::new()
                                } else {
                                    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
                                }
                            };
                            let (prefix, level, text) =
                                (cstr(msg.prefix), cstr(msg.level), cstr(msg.text));
                            let trimmed = text.trim_end();
                            match level.as_str() {
                                "fatal" | "error" => log::error!("mpv/{prefix}: {trimmed}"),
                                "warn" => log::warn!("mpv/{prefix}: {trimmed}"),
                                "info" => log::info!("mpv/{prefix}: {trimmed}"),
                                _ => log::debug!("mpv/{prefix}: {trimmed}"),
                            }
                            sink(MpvEventPayload::LogMessage {
                                prefix,
                                level,
                                text,
                            });
                        }
                        MPV_EVENT_START_FILE => {
                            log::debug!("mpv: start-file");
                            sink(MpvEventPayload::StartFile)
                        }
                        MPV_EVENT_FILE_LOADED => {
                            log::info!("mpv: file loaded");
                            sink(MpvEventPayload::FileLoaded)
                        }
                        MPV_EVENT_END_FILE => {
                            let reason = if ev.data.is_null() {
                                "unknown".to_string()
                            } else {
                                let end = unsafe { &*(ev.data as *const MpvEventEndFile) };
                                match end.reason {
                                    MPV_END_FILE_REASON_EOF => "eof",
                                    MPV_END_FILE_REASON_STOP => "stop",
                                    MPV_END_FILE_REASON_QUIT => "quit",
                                    MPV_END_FILE_REASON_ERROR => "error",
                                    _ => "unknown",
                                }
                                .to_string()
                            };
                            log::info!("mpv: end-file ({reason})");
                            sink(MpvEventPayload::EndFile { reason });
                        }
                        MPV_EVENT_SEEK => sink(MpvEventPayload::Seek),
                        MPV_EVENT_PLAYBACK_RESTART => sink(MpvEventPayload::PlaybackRestart),
                        MPV_EVENT_QUEUE_OVERFLOW => {
                            log::warn!("mpv event queue overflowed; some events were dropped");
                        }
                        _ => {}
                    }

                    if shutting_down.load(Ordering::SeqCst) {
                        break;
                    }
                }
            })
            .expect("failed to spawn mpv event thread");

        EventPump { join: Some(join) }
    }

    /// Signal the event thread to stop. Call before dropping the core.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        unsafe { (self.lib.wakeup)(self.handle) };
    }
}

impl Drop for MpvCore {
    fn drop(&mut self) {
        // The render context must already be gone: `mpv_render_context_free` has to happen
        // before the core is destroyed, and `MpvInstance` enforces that ordering.
        unsafe { (self.lib.terminate_destroy)(self.handle) };
    }
}

/// Wrapper making a raw handle movable into the event thread. Sound because the client API is
/// thread-safe and the handle outlives the thread (we join before destroying).
struct SendHandle(*mut MpvHandle);
unsafe impl Send for SendHandle {}

pub struct EventPump {
    join: Option<std::thread::JoinHandle<()>>,
}

impl EventPump {
    pub fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
