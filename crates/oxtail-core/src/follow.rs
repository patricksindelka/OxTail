//! Change detection for followed files (PLAN.md §5.5).
//!
//! Two cooperating mechanisms feed the document actor:
//!
//! * [`FsWatcher`]: a `notify` watcher on the parent directory that *pokes*
//!   the actor promptly (low latency). Events are only a hint.
//! * Adaptive polling ([`AdaptiveInterval`]): 250 ms while things change,
//!   backing off to about 1 s when idle. Polling always runs, because file
//!   system events are not reliable on network shares.
//!
//! Every poke or tick ends in [`classify`], a pure function that turns
//! "what the path names now" plus sizes into a [`Change`].

use std::io;
use std::path::Path;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::source::{PathState, ReadAt};

/// What to follow when a log is rotated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowMode {
    /// Follow the *name*: when the path starts naming a different file,
    /// reopen it (like `tail -F`). This is the default.
    #[default]
    Name,
    /// Follow the open *handle*, even if the file is renamed or replaced
    /// (like `tail -f`).
    Handle,
}

/// The outcome of one change-detection poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing happened.
    Unchanged,
    /// The file is longer than before (new length).
    Grew(u64),
    /// The file is shorter than before (new length): truncation.
    Truncated(u64),
    /// The path names a different file now (rotation); reopen it.
    Rotated,
    /// The path does not exist right now.
    Removed,
}

/// Decides what happened, given the followed path's state, the length we last
/// consumed and the current length of the open file.
pub fn classify(mode: FollowMode, path: PathState, known_len: u64, current_len: u64) -> Change {
    if mode == FollowMode::Name {
        match path {
            PathState::Missing => return Change::Removed,
            PathState::Different => return Change::Rotated,
            PathState::Same => {}
        }
    }
    match current_len.cmp(&known_len) {
        std::cmp::Ordering::Greater => Change::Grew(current_len),
        std::cmp::Ordering::Less => Change::Truncated(current_len),
        std::cmp::Ordering::Equal => Change::Unchanged,
    }
}

/// Number of bytes sampled at each end of a [`Fingerprint`].
const FINGERPRINT_LEN: usize = 64;

/// A cheap check that the bytes we already consumed are still there.
///
/// Comparing sizes cannot see a file that was truncated and rewritten to at
/// least its old length between two polls (copytruncate plus quick regrowth,
/// a `>` redirect). The fingerprint holds the first 64 bytes and the 64 bytes
/// ending at the last known length; two small positioned reads per poll tell
/// whether they are unchanged. Writers that pre-allocate a file with zeros
/// and fill it later would look rewritten; the cost of that false positive is
/// one view rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    len: u64,
    head: Vec<u8>,
    tail_off: u64,
    tail: Vec<u8>,
}

fn read_exact_or_short(src: &dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match src.read_at(offset + got as u64, &mut buf[got..])? {
            0 => return Ok(false),
            n => got += n,
        }
    }
    Ok(true)
}

impl Fingerprint {
    /// Samples the start of `src` and the bytes ending at `len`. Fails with
    /// [`io::ErrorKind::UnexpectedEof`] if the source is shorter than `len`.
    pub fn capture(src: &dyn ReadAt, len: u64) -> io::Result<Self> {
        let n = len.min(FINGERPRINT_LEN as u64) as usize;
        let mut head = vec![0u8; n];
        let tail_off = len - n as u64;
        let mut tail = vec![0u8; n];
        if !read_exact_or_short(src, 0, &mut head)?
            || !read_exact_or_short(src, tail_off, &mut tail)?
        {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(Self {
            len,
            head,
            tail_off,
            tail,
        })
    }

    /// The length this fingerprint was taken at.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// `true` for the fingerprint of an empty source.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `true` if `src` still holds the sampled bytes (`false` also when it
    /// became shorter than the sampled region).
    pub fn still_matches(&self, src: &dyn ReadAt) -> io::Result<bool> {
        let mut head = vec![0u8; self.head.len()];
        if !read_exact_or_short(src, 0, &mut head)? || head != self.head {
            return Ok(false);
        }
        let mut tail = vec![0u8; self.tail.len()];
        Ok(read_exact_or_short(src, self.tail_off, &mut tail)? && tail == self.tail)
    }
}

/// Poll interval that backs off when nothing changes.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveInterval {
    min: Duration,
    max: Duration,
    cur: Duration,
}

impl Default for AdaptiveInterval {
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(1))
    }
}

impl AdaptiveInterval {
    /// Creates an interval starting at `min` and never exceeding `max`.
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max: max.max(min),
            cur: min,
        }
    }

    /// The current wait between polls.
    pub fn current(&self) -> Duration {
        self.cur
    }

    /// Something changed: poll quickly again.
    pub fn on_activity(&mut self) {
        self.cur = self.min;
    }

    /// Nothing changed: back off (x1.5, capped).
    pub fn on_idle(&mut self) {
        self.cur = (self.cur + self.cur / 2).min(self.max);
    }
}

/// A `notify` watcher that calls `wake` when something happens to `path`.
///
/// Dropping it stops watching. Creation failures are non-fatal (polling
/// continues), so [`FsWatcher::new`] returns `None` then.
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
}

impl FsWatcher {
    /// Watches the parent directory of `path` (so creation/rename of the path
    /// is seen too) and calls `wake` for events that mention the file name.
    pub fn new(path: &Path, wake: impl Fn() + Send + 'static) -> Option<Self> {
        let name = path.file_name()?.to_owned();
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => Path::new(".").to_path_buf(),
        };
        // FSEvents does not deliver events for files reached through a
        // symlinked directory (macOS temp dirs live under `/var` ->
        // `/private/var`), so watch the real directory there.
        #[cfg(target_os = "macos")]
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(ev) => {
                    if ev
                        .paths
                        .iter()
                        .any(|p| p.file_name() == Some(name.as_os_str()))
                    {
                        wake();
                    }
                }
                // Overflow or backend errors: poke anyway, polling sorts it out.
                Err(_) => wake(),
            }
        })
        .map_err(|e| tracing::debug!("notify unavailable: {e}"))
        .ok()?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(|e| tracing::debug!("cannot watch {}: {e}", dir.display()))
            .ok()?;
        Some(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_all_cases() {
        use Change::*;
        use FollowMode::*;
        use PathState::*;
        assert_eq!(classify(Name, Same, 10, 10), Unchanged);
        assert_eq!(classify(Name, Same, 10, 20), Grew(20));
        assert_eq!(classify(Name, Same, 10, 3), Truncated(3));
        assert_eq!(classify(Name, Same, 10, 0), Truncated(0));
        assert_eq!(classify(Name, Different, 10, 20), Rotated);
        assert_eq!(classify(Name, Missing, 10, 10), Removed);
        // Handle mode ignores the path.
        assert_eq!(classify(Handle, Different, 10, 20), Grew(20));
        assert_eq!(classify(Handle, Missing, 10, 10), Unchanged);
    }

    #[test]
    fn fingerprint_detects_rewrites_but_not_appends() {
        use crate::source::MemSource;
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let src = MemSource::new(data.clone());
        let fp = Fingerprint::capture(&src, 1000).unwrap();
        assert_eq!(fp.len(), 1000);
        assert!(fp.still_matches(&src).unwrap());
        src.append(b"more data\n");
        assert!(fp.still_matches(&src).unwrap(), "append keeps it valid");
        // Change one byte in the middle: not sampled, not noticed (by design).
        let mut mid = data.clone();
        mid[500] ^= 0xff;
        src.replace(mid);
        assert!(fp.still_matches(&src).unwrap());
        // Head, tail and shrinking are noticed.
        let mut head = data.clone();
        head[3] ^= 1;
        src.replace(head);
        assert!(!fp.still_matches(&src).unwrap());
        let mut tail = data.clone();
        tail[990] ^= 1;
        src.replace(tail);
        assert!(!fp.still_matches(&src).unwrap());
        src.replace(data[..600].to_vec());
        assert!(!fp.still_matches(&src).unwrap());
        // Short and empty sources.
        let small = MemSource::new(b"abc".to_vec());
        let fp = Fingerprint::capture(&small, 3).unwrap();
        assert!(fp.still_matches(&small).unwrap());
        small.replace(b"abd".to_vec());
        assert!(!fp.still_matches(&small).unwrap());
        assert!(Fingerprint::capture(&small, 10).is_err());
        assert!(
            Fingerprint::capture(&small, 0)
                .unwrap()
                .still_matches(&small)
                .unwrap()
        );
    }

    #[test]
    fn interval_backs_off_and_resets() {
        let mut i = AdaptiveInterval::default();
        assert_eq!(i.current(), Duration::from_millis(250));
        for _ in 0..20 {
            i.on_idle();
        }
        assert_eq!(i.current(), Duration::from_secs(1));
        i.on_activity();
        assert_eq!(i.current(), Duration::from_millis(250));
    }

    /// Appends to `path` until `hits` moves or 10 s pass.
    fn append_until_poked(path: &std::path::Path, hits: &std::sync::atomic::AtomicUsize) {
        use std::io::Write;
        use std::sync::atomic::Ordering;
        use std::time::Instant;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            f.write_all(b"more\n").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // Appends to an already-open file produce no watcher event on macOS
    // (FSEvents, seen in CI on macos-latest) nor on Windows (NTFS updates the
    // directory entry's size/time lazily while a handle is open, so
    // ReadDirectoryChangesW stays quiet). Following still works there via the
    // adaptive poll (tests/follow.rs passes on both); instant wake-ups on
    // append are a known gap, see docs/dev.md.
    #[cfg_attr(
        any(target_os = "macos", windows),
        ignore = "no append events for open files on this OS; polling covers it"
    )]
    #[cfg(unix)]
    #[test]
    fn watcher_pokes_through_a_symlinked_directory() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("w.log"), b"x\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let Some(_w) = FsWatcher::new(&link.join("w.log"), move || {
            h.fetch_add(1, Ordering::SeqCst);
        }) else {
            return; // no notify backend in this sandbox; polling covers it
        };
        append_until_poked(&real.join("w.log"), &hits);
        assert!(
            hits.load(Ordering::SeqCst) > 0,
            "watcher never fired through a symlinked directory"
        );
    }

    // Appends to an already-open file produce no watcher event on macOS
    // (FSEvents, seen in CI on macos-latest) nor on Windows (NTFS updates the
    // directory entry's size/time lazily while a handle is open, so
    // ReadDirectoryChangesW stays quiet). Following still works there via the
    // adaptive poll (tests/follow.rs passes on both); instant wake-ups on
    // append are a known gap, see docs/dev.md.
    #[cfg_attr(
        any(target_os = "macos", windows),
        ignore = "no append events for open files on this OS; polling covers it"
    )]
    #[test]
    fn watcher_pokes_on_append() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.log");
        std::fs::write(&path, b"x\n").unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let Some(_w) = FsWatcher::new(&path, move || {
            h.fetch_add(1, Ordering::SeqCst);
        }) else {
            return; // no notify backend in this sandbox; polling covers it
        };
        append_until_poked(&path, &hits);
        assert!(hits.load(Ordering::SeqCst) > 0, "watcher never fired");
    }
}
