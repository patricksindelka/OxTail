//! Random-access byte sources.

use std::io;
use std::sync::Arc;

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
