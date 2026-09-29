//! Spool files: the UTF-8 view of files that are not UTF-8 on disk.
//!
//! A spool is a temp file that receives the transcoded bytes of the raw file,
//! appended incrementally as the raw file grows. It is deleted when the
//! [`Spool`] is dropped (which happens when the `Document` is dropped).

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use tempfile::NamedTempFile;

use crate::encoding::{LineEnding, TextEncoding, Transcoder};
use crate::error::CoreError;
use crate::source::{FileSource, ReadAt};

/// File-name prefix of every temp file this crate creates in the spool
/// directory (transcoding spools and stdin buffers). A startup cleanup of
/// crashed sessions only deletes files with this prefix.
///
/// **Must match `oxtail_config::datadir::SPOOL_PREFIX`** (this crate must not
/// depend on `oxtail-config`, so the value is duplicated).
pub const SPOOL_PREFIX: &str = "oxtail-spool-";

/// Prefix of the stdin buffer files (starts with [`SPOOL_PREFIX`]).
pub(crate) const STDIN_SPOOL_PREFIX: &str = "oxtail-spool-stdin-";

/// Creates a named temp file in `dir`, falling back to the system temp dir.
pub(crate) fn create_temp(dir: Option<&Path>, prefix: &str) -> Result<NamedTempFile, CoreError> {
    let mut b = tempfile::Builder::new();
    b.prefix(prefix).suffix(".spool");
    if let Some(dir) = dir {
        match b.tempfile_in(dir) {
            Ok(f) => return Ok(f),
            Err(e) => tracing::warn!("spool dir {} unusable ({e}); using temp dir", dir.display()),
        }
    }
    b.tempfile()
        .map_err(|e| CoreError::io(std::env::temp_dir(), e))
}

/// Incrementally transcoded UTF-8 copy of a raw file.
pub(crate) struct Spool {
    tmp: NamedTempFile,
    reader: Arc<FileSource>,
    transcoder: Transcoder,
    len: u64,
    scratch: Vec<u8>,
    /// Set by a failed write: the transcoder has advanced past bytes that are
    /// not (fully) in the file, so the spool can only be discarded.
    poisoned: bool,
    /// Test hook: number of upcoming appends that write half and then fail.
    #[cfg(test)]
    pub(crate) fault: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl Spool {
    /// Creates an empty spool.
    pub(crate) fn new(
        dir: Option<&Path>,
        encoding: TextEncoding,
        line_ending: LineEnding,
    ) -> Result<Self, CoreError> {
        let tmp = create_temp(dir, SPOOL_PREFIX)?;
        let reader_file: File = tmp
            .reopen()
            .map_err(|e| CoreError::io(tmp.path().to_path_buf(), e))?;
        Ok(Self {
            reader: Arc::new(FileSource::from_file(reader_file)),
            tmp,
            transcoder: Transcoder::new(encoding, line_ending),
            len: 0,
            scratch: Vec::new(),
            poisoned: false,
            #[cfg(test)]
            fault: None,
        })
    }

    /// The readable UTF-8 view.
    pub(crate) fn source(&self) -> Arc<dyn ReadAt> {
        self.reader.clone()
    }

    /// Bytes written so far (the length of the UTF-8 view).
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// `true` after a failed write. A poisoned spool refuses further appends
    /// and must be replaced (transcoder state cannot be rewound).
    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Transcodes `raw` (the next bytes of the raw file) and appends the result.
    ///
    /// On a write error the spool is poisoned and the file is cut back to
    /// [`Spool::len`] (best effort) so readers never see bytes the length does
    /// not cover.
    pub(crate) fn append(&mut self, raw: &[u8]) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "spool is poisoned by an earlier write error",
            ));
        }
        self.scratch.clear();
        self.transcoder.transcode(raw, &mut self.scratch);
        if self.scratch.is_empty() {
            return Ok(());
        }
        match self.write_scratch() {
            Ok(()) => {
                self.len += self.scratch.len() as u64;
                Ok(())
            }
            Err(e) => {
                self.poisoned = true;
                let _ = self.tmp.as_file().set_len(self.len);
                Err(e)
            }
        }
    }

    fn write_scratch(&self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(fault) = &self.fault {
            use std::sync::atomic::Ordering;
            let armed = fault
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if armed {
                let mut f = self.tmp.as_file();
                f.write_all(&self.scratch[..self.scratch.len() / 2])?;
                return Err(io::Error::other("injected write failure"));
            }
        }
        let mut f = self.tmp.as_file();
        f.write_all(&self.scratch)
    }

    /// Location of the spool file (for tests).
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        self.tmp.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_delete_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Spool::new(Some(dir.path()), TextEncoding::UTF_16LE, LineEnding::Lf).unwrap();
        let path = s.path().to_path_buf();
        assert!(path.starts_with(dir.path()));
        // "hi\n" in UTF-16LE, the last code unit split across appends
        s.append(&[b'h', 0, b'i', 0, b'\n']).unwrap();
        s.append(&[0]).unwrap();
        assert_eq!(s.len(), 3);
        let mut buf = [0u8; 8];
        let n = s.source().read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi\n");
        drop(s);
        assert!(!path.exists());
    }

    #[test]
    fn spool_files_carry_the_shared_prefix() {
        assert!(STDIN_SPOOL_PREFIX.starts_with(SPOOL_PREFIX));
        let dir = tempfile::tempdir().unwrap();
        let s = Spool::new(Some(dir.path()), TextEncoding::UTF_16LE, LineEnding::Lf).unwrap();
        let name = s.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(SPOOL_PREFIX), "{name}");
        let t = create_temp(Some(dir.path()), STDIN_SPOOL_PREFIX).unwrap();
        let name = t.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(SPOOL_PREFIX), "{name}");
    }

    #[test]
    fn falls_back_when_dir_is_unusable() {
        let s = Spool::new(
            Some(Path::new("/definitely/not/a/dir")),
            TextEncoding::UTF_8,
            LineEnding::Cr,
        )
        .unwrap();
        assert!(s.path().exists());
    }
}
