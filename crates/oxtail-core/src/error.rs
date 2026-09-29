//! Error type for `oxtail-core`.

use std::path::PathBuf;

/// Errors produced by `oxtail-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// An I/O operation on a file failed.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The file involved (may be a spool file).
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// An I/O operation without a meaningful path failed.
    #[error("I/O error: {0}")]
    Read(#[from] std::io::Error),
    /// An encoding label was not recognised.
    #[error("unknown encoding: {0}")]
    UnknownEncoding(String),
    /// The background actor thread could not be started or has stopped.
    #[error("document actor unavailable: {0}")]
    Actor(String),
}

impl CoreError {
    /// Wraps an I/O error together with the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }
}
