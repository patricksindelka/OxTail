//! `tracing` setup: stderr always, plus `<data>/oxtail.log` when the data
//! folder is persistent. The file is rotated once it exceeds 1 MB (the old
//! content moves to `oxtail.log.1`), so it can never grow without bound.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Size at which the log file is rotated.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Name of the log file inside the data folder.
pub const LOG_FILE: &str = "oxtail.log";

/// Rotates `path` if it is larger than `max` bytes and opens it for append.
pub fn open_log_file(path: &Path, max: u64) -> std::io::Result<File> {
    if fs::metadata(path).map(|m| m.len() > max).unwrap_or(false) {
        let mut old: PathBuf = path.as_os_str().to_owned().into();
        old.set_extension("log.1");
        // Best effort: if the rename fails, truncate instead.
        if fs::rename(path, &old).is_err() {
            let _ = fs::remove_file(path);
        }
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// A log file shared between all writers of the subscriber.
#[derive(Clone)]
struct SharedFile(Arc<Mutex<File>>);

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut f) => f.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.lock() {
            Ok(mut f) => f.flush(),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for SharedFile {
    type Writer = SharedFile;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Installs the global subscriber. Safe to call once; later calls are no-ops.
pub fn init(data_root: Option<&Path>) {
    let filter = EnvFilter::try_from_env("OXTAIL_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);
    let file_layer = data_root
        .and_then(|root| open_log_file(&root.join(LOG_FILE), MAX_LOG_BYTES).ok())
        .map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(SharedFile(Arc::new(Mutex::new(file))))
        });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file_layer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_files_are_appended() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LOG_FILE);
        fs::write(&p, b"abc").unwrap();
        let _f = open_log_file(&p, 100).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"abc");
        assert!(!dir.path().join("oxtail.log.1").exists());
    }

    #[test]
    fn big_files_are_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LOG_FILE);
        fs::write(&p, vec![b'x'; 200]).unwrap();
        let _f = open_log_file(&p, 100).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
        assert_eq!(
            fs::metadata(dir.path().join("oxtail.log.1")).unwrap().len(),
            200
        );
    }
}
