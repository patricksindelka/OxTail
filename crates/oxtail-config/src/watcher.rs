//! Hot reload: debounced notifications when configuration files change.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::atomic::TEMP_SUFFIX;

/// Which kind of configuration changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigKind {
    /// `settings.toml`.
    Settings,
    /// Something in `profiles/`.
    Profiles,
    /// Something in `themes/`.
    Themes,
}

/// A configuration change notification (already debounced: a burst of writes
/// yields one message per kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigChanged(pub ConfigKind);

/// Watches a data folder and sends [`ConfigChanged`] messages. Dropping it
/// stops the watch and its helper thread.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

fn classify(root: &Path, path: &Path) -> Option<ConfigKind> {
    if path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(TEMP_SUFFIX))
    {
        return None;
    }
    let rel = path.strip_prefix(root).ok()?;
    let first = rel.components().next()?.as_os_str().to_string_lossy();
    match &*first {
        "settings.toml" => Some(ConfigKind::Settings),
        "profiles" => Some(ConfigKind::Profiles),
        "themes" => Some(ConfigKind::Themes),
        _ => None,
    }
}

impl ConfigWatcher {
    /// Starts watching `root`. `debounce` is the quiet time after the last
    /// event before notifying (bursts are also flushed after ten times that).
    /// Notifications arrive on the returned channel; the watcher runs on its
    /// own threads and never blocks the caller.
    pub fn new(
        root: &Path,
        debounce: Duration,
    ) -> Result<(ConfigWatcher, Receiver<ConfigChanged>), notify::Error> {
        // Resolve symlinks so event paths (which are canonical on some
        // platforms) still match the prefix.
        let root: PathBuf = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let (raw_tx, raw_rx): (Sender<ConfigKind>, Receiver<ConfigKind>) = unbounded();
        let (out_tx, out_rx) = unbounded();

        let cb_root = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(ev) = res else { return };
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for p in &ev.paths {
                    if let Some(k) = classify(&cb_root, p) {
                        let _ = raw_tx.send(k);
                    }
                }
            })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;

        thread::Builder::new()
            .name("oxtail-config-watch".into())
            .spawn(move || {
                let mut pending: BTreeSet<ConfigKind> = BTreeSet::new();
                let mut first_event: Option<Instant> = None;
                loop {
                    let timeout = if pending.is_empty() {
                        Duration::from_secs(3600)
                    } else {
                        debounce
                    };
                    match raw_rx.recv_timeout(timeout) {
                        Ok(k) => {
                            pending.insert(k);
                            let start = *first_event.get_or_insert_with(Instant::now);
                            if start.elapsed() < debounce.saturating_mul(10) {
                                continue;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    first_event = None;
                    for k in std::mem::take(&mut pending) {
                        if out_tx.send(ConfigChanged(k)).is_err() {
                            return;
                        }
                    }
                }
            })
            .map_err(|e| notify::Error::generic(&e.to_string()))?;

        Ok((ConfigWatcher { _watcher: watcher }, out_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Waits up to 10 s for a message of `kind`.
    fn wait_for(rx: &Receiver<ConfigChanged>, kind: ConfigKind) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(ConfigChanged(k)) if k == kind => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn classify_paths() {
        let root = Path::new("/d");
        assert_eq!(
            classify(root, Path::new("/d/settings.toml")),
            Some(ConfigKind::Settings)
        );
        assert_eq!(
            classify(root, Path::new("/d/profiles/a.toml")),
            Some(ConfigKind::Profiles)
        );
        assert_eq!(
            classify(root, Path::new("/d/themes/x/y.toml")),
            Some(ConfigKind::Themes)
        );
        assert_eq!(classify(root, Path::new("/d/session.json")), None);
        assert_eq!(
            classify(root, Path::new("/d/.settings.toml.1.0.oxtail-tmp")),
            None
        );
        assert_eq!(classify(root, Path::new("/other/settings.toml")), None);
    }

    #[test]
    fn notifies_on_settings_and_profile_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("profiles")).expect("mkdir");
        let (_w, rx) = ConfigWatcher::new(dir.path(), Duration::from_millis(50)).expect("watcher");
        crate::write_atomic(&dir.path().join("settings.toml"), b"wrap = true\n").expect("write");
        assert!(wait_for(&rx, ConfigKind::Settings), "no settings event");
        fs::write(dir.path().join("profiles/p.toml"), "name = \"p\"\n").expect("write");
        assert!(wait_for(&rx, ConfigKind::Profiles), "no profiles event");
    }

    #[test]
    fn bursts_are_debounced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_w, rx) = ConfigWatcher::new(dir.path(), Duration::from_millis(300)).expect("watcher");
        for i in 0..10 {
            fs::write(
                dir.path().join("settings.toml"),
                format!("font_size = {}\n", 10 + i),
            )
            .expect("write");
        }
        assert!(wait_for(&rx, ConfigKind::Settings));
        // Nothing more for a while: the burst produced a single message.
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
    }

    #[test]
    fn dropping_stops_delivery() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (w, rx) = ConfigWatcher::new(dir.path(), Duration::from_millis(20)).expect("watcher");
        drop(w);
        // The helper thread ends and the channel disconnects (poll, bounded).
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Err(RecvTimeoutError::Disconnected) => break,
                _ if Instant::now() > deadline => panic!("channel never disconnected"),
                _ => {}
            }
        }
    }
}
