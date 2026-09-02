//! Typed errors. Every variant is serializable so it can cross the Tauri command boundary and be
//! rendered in the frontend, and `Clone` so the one-time libmpv load result can be shared.

use serde::{Serialize, Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error(
        "libmpv could not be loaded. Install mpv, or set {env} to the library path. Tried: {attempts}",
        env = crate::mpv::loader::LIBMPV_PATH_ENV
    )]
    MpvNotFound { attempts: String },

    #[error("libmpv is missing the symbol `{symbol}` ({reason}); the library is likely too old or not libmpv")]
    MpvSymbolMissing { symbol: String, reason: String },

    #[error(
        "libmpv client API {found} is too old; this plugin needs at least {required} (mpv 0.33+)"
    )]
    MpvTooOld { found: String, required: String },

    #[error("mpv error {code}: {message}")]
    Mpv { code: i32, message: String },

    #[error("mpv is not initialized for window `{label}`; call init() first")]
    NotInitialized { label: String },

    #[error("mpv is already initialized for window `{label}`")]
    AlreadyInitialized { label: String },

    #[error("no window labelled `{label}`")]
    NoSuchWindow { label: String },

    #[error("failed to create the video surface: {0}")]
    Surface(String),

    #[error("failed to initialize the mpv render context: {0}")]
    RenderInit(String),

    #[error("this platform is not supported by tauri-plugin-mpv-surface")]
    UnsupportedPlatform,

    #[error("property `{name}` holds a value this plugin cannot represent as JSON (mpv format {format})")]
    UnsupportedFormat { name: String, format: i32 },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error(transparent)]
    Tauri(#[from] TauriErrorString),
}

/// `tauri::Error` is neither `Clone` nor `Serialize`, so it is flattened to its message on the
/// way in. Keeping `Error: Clone` is what lets the cached libmpv load result be handed out
/// repeatedly rather than reloaded.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct TauriErrorString(pub String);

impl From<tauri::Error> for Error {
    fn from(e: tauri::Error) -> Self {
        Error::Tauri(TauriErrorString(e.to_string()))
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        // A tagged shape so the frontend can branch on `kind` (e.g. show install instructions
        // for MpvNotFound) rather than string-matching the message.
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Error", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

impl Error {
    pub fn kind(&self) -> &'static str {
        match self {
            Error::MpvNotFound { .. } => "MpvNotFound",
            Error::MpvSymbolMissing { .. } => "MpvSymbolMissing",
            Error::MpvTooOld { .. } => "MpvTooOld",
            Error::Mpv { .. } => "Mpv",
            Error::NotInitialized { .. } => "NotInitialized",
            Error::AlreadyInitialized { .. } => "AlreadyInitialized",
            Error::NoSuchWindow { .. } => "NoSuchWindow",
            Error::Surface(_) => "Surface",
            Error::RenderInit(_) => "RenderInit",
            Error::UnsupportedPlatform => "UnsupportedPlatform",
            Error::UnsupportedFormat { .. } => "UnsupportedFormat",
            Error::InvalidArgument(_) => "InvalidArgument",
            Error::Tauri(_) => "Tauri",
        }
    }
}
