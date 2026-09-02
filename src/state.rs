//! Plugin state: one mpv instance per window.
//!
//! Only `Send + Sync` things live here. The GL surface and the mpv render context are
//! thread-affine and live in the platform module's main-thread registry instead; this state
//! reaches them through `run_on_main_thread`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mpv::core::{EventPump, MpvCore};

/// Options passed from the frontend when creating an mpv instance.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MpvConfig {
    /// mpv options applied before initialization, e.g. `{"hwdec": "auto-safe"}`.
    /// `vo` is ignored: the render API requires `vo=libmpv`.
    pub options: BTreeMap<String, String>,
    /// Properties to observe immediately, delivered as `property-change` events.
    pub observe: Vec<String>,
    /// mpv log level forwarded to the host logger and to `log-message` events:
    /// `no`, `fatal`, `error`, `warn`, `info`, `v`, `debug`, `trace`.
    pub log_level: Option<String>,
    /// Enable MPV_RENDER_PARAM_ADVANCED_CONTROL. Off by default.
    pub advanced_control: Option<bool>,
    /// Override the framebuffer vertical orientation. Leave unset unless video renders
    /// upside down — the per-platform default is correct for that platform's framebuffer.
    pub flip_y: Option<bool>,
}

pub struct MpvInstance {
    pub core: Arc<MpvCore>,
    pump: Mutex<Option<EventPump>>,
    next_id: AtomicU64,
    observers: Mutex<HashMap<String, u64>>,
}

impl MpvInstance {
    pub fn new(core: Arc<MpvCore>, pump: EventPump) -> Self {
        MpvInstance {
            core,
            pump: Mutex::new(Some(pump)),
            next_id: AtomicU64::new(1),
            observers: Mutex::new(HashMap::new()),
        }
    }

    pub fn observe(&self, name: &str) -> Result<()> {
        let mut observers = self.observers.lock().unwrap();
        if observers.contains_key(name) {
            return Ok(());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.core.observe_property(id, name)?;
        observers.insert(name.to_string(), id);
        Ok(())
    }

    pub fn unobserve(&self, name: &str) -> Result<()> {
        let mut observers = self.observers.lock().unwrap();
        if let Some(id) = observers.remove(name) {
            self.core.unobserve_property(id)?;
        }
        Ok(())
    }

    /// Stop the event pump and wait for the thread to exit.
    pub fn shutdown(&self) {
        self.core.begin_shutdown();
        if let Some(mut pump) = self.pump.lock().unwrap().take() {
            pump.join();
        }
    }
}

#[derive(Default)]
pub struct MpvState {
    instances: Mutex<HashMap<String, Arc<MpvInstance>>>,
}

impl MpvState {
    pub fn insert(&self, label: String, instance: Arc<MpvInstance>) {
        self.instances.lock().unwrap().insert(label, instance);
    }

    pub fn get(&self, label: &str) -> Result<Arc<MpvInstance>> {
        self.instances
            .lock()
            .unwrap()
            .get(label)
            .cloned()
            .ok_or_else(|| Error::NotInitialized {
                label: label.to_string(),
            })
    }

    pub fn contains(&self, label: &str) -> bool {
        self.instances.lock().unwrap().contains_key(label)
    }

    pub fn remove(&self, label: &str) -> Option<Arc<MpvInstance>> {
        self.instances.lock().unwrap().remove(label)
    }
}
