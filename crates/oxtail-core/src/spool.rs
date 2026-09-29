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
}

impl Spool {
    /// Creates an empty spool.
    pub(crate) fn new(
        dir: Option<&Path>,
        encoding: TextEncoding,
        line_ending: LineEnding,
    ) -> Result<Self, CoreError> {
        let tmp = create_temp(dir, "oxtail-")?;
        let reader_file: File = tmp
            .reopen()
            .map_err(|e| CoreError::io(tmp.path().to_path_buf(), e))?;
        Ok(Self {
            reader: Arc::new(FileSource::from_file(reader_file)),
            tmp,
            transcoder: Transcoder::new(encoding, line_ending),
            len: 0,
            scratch: Vec::new(),
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

    /// Transcodes `raw` (the next bytes of the raw file) and appends the result.
    pub(crate) fn append(&mut self, raw: &[u8]) -> io::Result<()> {
        self.scratch.clear();
        self.transcoder.transcode(raw, &mut self.scratch);
        if !self.scratch.is_empty() {
            let mut f = self.tmp.as_file();
            f.write_all(&self.scratch)?;
            self.len += self.scratch.len() as u64;
        }
        Ok(())
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
