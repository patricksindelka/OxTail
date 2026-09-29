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

use std::path::Path;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::source::PathState;

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

    #[test]
    fn watcher_pokes_on_append() {
        use std::io::Write;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

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
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            f.write_all(b"more\n").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(hits.load(Ordering::SeqCst) > 0, "watcher never fired");
    }
}
