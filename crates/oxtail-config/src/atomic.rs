//! Atomic file replacement.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Suffix of temporary files created by [`write_atomic`]; leftovers of a crash
/// can be recognised (and removed) by it.
pub const TEMP_SUFFIX: &str = ".oxtail-tmp";

fn temp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".{name}.{}.{n}{TEMP_SUFFIX}", std::process::id());
    path.with_file_name(tmp)
}

/// Replaces `path` with `bytes` atomically: the data is written to a temporary
/// file in the same directory, flushed to disk (`fsync`) and renamed over the
/// target, so a reader (or a pulled USB stick) sees either the old or the new
/// content, never a torn file.
///
/// On Windows a rename onto a momentarily locked target (antivirus, indexer,
/// a reader without `FILE_SHARE_DELETE`) is retried for about a second.
/// The parent directory must exist.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path_for(path);
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        rename_with_retry(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    } else {
        sync_parent(path);
    }
    result
}

fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let attempts = if cfg!(windows) { 20 } else { 1 };
    let mut last = None;
    for i in 0..attempts {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let retry = cfg!(windows)
                    && matches!(
                        e.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                    );
                last = Some(e);
                if !retry {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10 + 5 * i));
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Best effort: persist the rename itself on Unix.
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_replaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("a.toml");
        write_atomic(&p, b"one").expect("write");
        assert_eq!(fs::read(&p).expect("read"), b"one");
        write_atomic(&p, b"two-longer").expect("write");
        assert_eq!(fs::read(&p).expect("read"), b"two-longer");
        write_atomic(&p, b"x").expect("write");
        assert_eq!(fs::read(&p).expect("read"), b"x");
        // No temp files are left behind.
        let left: Vec<_> = fs::read_dir(dir.path())
            .expect("readdir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn missing_parent_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(write_atomic(&dir.path().join("no/such/x"), b"1").is_err());
    }
}
