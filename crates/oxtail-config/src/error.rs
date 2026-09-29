//! Error type for saving and loading configuration.

use std::path::PathBuf;

/// Errors from reading or writing configuration files.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A TOML file could not be parsed.
    #[error("{path}: invalid TOML: {source}")]
    TomlParse {
        /// The file involved.
        path: PathBuf,
        /// The parse error (with line and column).
        #[source]
        source: toml::de::Error,
    },
    /// A value could not be serialised as TOML.
    #[error("cannot serialise TOML: {0}")]
    TomlWrite(#[from] toml::ser::Error),
    /// A JSON file could not be parsed or written.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The data folder is in-memory mode, so nothing can be saved.
    #[error("settings are not persisted: {0}")]
    NotPersistent(String),
    /// Registering with (or removing from) the operating system failed.
    #[error("system integration: {0}")]
    Integration(String),
    /// The update check could not be completed (no `curl`, no network, bad
    /// answer). A missing release or an unparseable version is not an error.
    #[error("update check: {0}")]
    Update(String),
}

impl ConfigError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
