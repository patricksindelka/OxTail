//! Random-access byte sources.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

/// A random-access, shareable view of bytes (a file, a spool, or memory).
///
/// Implementations must be cheap to call from many threads at once and must
/// never block other processes from writing, truncating or deleting the
/// underlying file.
pub trait ReadAt: Send + Sync {
    /// Reads up to `buf.len()` bytes starting at `offset`. Returns the number
    /// of bytes read; `0` means `offset` is at or past the current end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Current length in bytes. May grow (or shrink) between calls.
    fn len(&self) -> io::Result<u64>;

    /// Returns `true` if the source is currently empty.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Reads exactly `buf.len()` bytes at `offset`, looping over short reads.
    /// Fails with [`io::ErrorKind::UnexpectedEof`] if the source ends first.
    fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(offset, buf)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => {
                    offset += n as u64;
                    buf = &mut buf[n..];
                }
            }
        }
        Ok(())
    }
}

impl<T: ReadAt + ?Sized> ReadAt for Arc<T> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn len(&self) -> io::Result<u64> {
        (**self).len()
    }
}

/// An in-memory [`ReadAt`] whose contents can be appended to or replaced,
/// which makes it useful for tests that simulate a growing or truncated log.
#[derive(Debug, Default)]
pub struct MemSource {
    data: std::sync::RwLock<Vec<u8>>,
}

impl MemSource {
    /// Creates a source holding `data`.
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: std::sync::RwLock::new(data.into()),
        }
    }

    /// Appends bytes, as a log writer would.
    pub fn append(&self, bytes: &[u8]) {
        self.data
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(bytes);
    }

    /// Replaces all contents (simulates truncation or rotation).
    pub fn replace(&self, bytes: impl Into<Vec<u8>>) {
        *self.data.write().unwrap_or_else(|e| e.into_inner()) = bytes.into();
    }
}

impl ReadAt for MemSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.data.read().unwrap_or_else(|e| e.into_inner());
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        if start >= data.len() {
            return Ok(0);
        }
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.data.read().unwrap_or_else(|e| e.into_inner()).len() as u64)
    }
}

/// Opens `path` read-only, never blocking writers, rotators or deleters.
///
/// On Windows the file is opened with `FILE_SHARE_READ | FILE_SHARE_WRITE |
/// FILE_SHARE_DELETE` so the process that writes the log can keep writing,
/// truncating, renaming and deleting it.
pub fn open_shared(path: &Path) -> io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ (1) | FILE_SHARE_WRITE (2) | FILE_SHARE_DELETE (4)
        opts.share_mode(1 | 2 | 4);
    }
    opts.open(path)
}

fn file_read_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        loop {
            match file.read_at(buf, offset) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => return r,
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_read(buf, offset)
    }
}

/// A read-only, positioned-read view of a file on disk (no mmap).
///
/// The underlying handle can be swapped with [`FileSource::reopen`] when the
/// path is rotated, while the `Arc<FileSource>` handed to readers stays valid.
#[derive(Debug)]
pub struct FileSource {
    path: Option<PathBuf>,
    inner: RwLock<Opened>,
}

#[derive(Debug)]
struct Opened {
    file: Arc<File>,
    id: Option<same_file::Handle>,
}

impl Opened {
    fn new(file: File) -> Self {
        let id = file.try_clone().and_then(same_file::Handle::from_file).ok();
        Opened {
            file: Arc::new(file),
            id,
        }
    }
}

/// Relationship between a followed path and the file currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathState {
    /// The path names the very same file we have open.
    Same,
    /// The path now names a different file (rotation).
    Different,
    /// The path does not exist (or cannot be opened) right now.
    Missing,
}

impl FileSource {
    /// Opens `path` read-only with shared access.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = open_shared(path)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            inner: RwLock::new(Opened::new(file)),
        })
    }

    /// Wraps an already opened file (e.g. a spool file). Rotation checks are
    /// not available for such a source.
    pub fn from_file(file: File) -> Self {
        Self {
            path: None,
            inner: RwLock::new(Opened::new(file)),
        }
    }

    /// The path this source was opened from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Re-opens the path, replacing the handle (used after rotation).
    /// On failure the old handle stays in place.
    pub fn reopen(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source has no path",
            ));
        };
        let file = open_shared(path)?;
        *self.inner.write() = Opened::new(file);
        Ok(())
    }

    /// Compares the identity of the open file (device+inode on Unix, volume
    /// serial + file index on Windows) with what the path names right now.
    pub fn path_state(&self) -> PathState {
        let Some(path) = &self.path else {
            return PathState::Same;
        };
        let Ok(now) = same_file::Handle::from_path(path) else {
            return PathState::Missing;
        };
        match &self.inner.read().id {
            Some(id) if *id == now => PathState::Same,
            Some(_) => PathState::Different,
            // Identity unavailable: assume unchanged rather than thrash.
            None => PathState::Same,
        }
    }
}

impl ReadAt for FileSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let file = self.inner.read().file.clone();
        file_read_at(&file, offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        let file = self.inner.read().file.clone();
        Ok(file.metadata()?.len())
    }
}

/// A [`ReadAt`] whose target can be swapped while readers hold an `Arc` to it.
/// The document's UTF-8 view is one of these so `Document::source()` stays
/// valid across encoding changes.
pub struct SwitchSource {
    inner: RwLock<Arc<dyn ReadAt>>,
}

impl SwitchSource {
    /// Creates a switchable source delegating to `inner`.
    pub fn new(inner: Arc<dyn ReadAt>) -> Self {
        Self {
            inner: RwLock::new(inner),
        }
    }

    /// Replaces the delegate.
    pub fn switch(&self, inner: Arc<dyn ReadAt>) {
        *self.inner.write() = inner;
    }
}

impl ReadAt for SwitchSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let inner = self.inner.read().clone();
        inner.read_at(offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        let inner = self.inner.read().clone();
        inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_source_reads_appends_and_truncates() {
        let src = MemSource::new(b"abc".to_vec());
        let mut buf = [0u8; 8];
        assert_eq!(src.read_at(1, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"bc");
        src.append(b"def");
        assert_eq!(src.len().unwrap(), 6);
        src.replace(b"x".to_vec());
        assert_eq!(src.read_at(3, &mut buf).unwrap(), 0);
        let mut exact = [0u8; 2];
        assert!(src.read_exact_at(0, &mut exact).is_err());
    }
}
