//! Configuration I/O on a worker thread: saving settings, session, profiles
//! and the search history, and reloading profiles, themes and settings after
//! the files changed on disk. The UI thread only sends [`Job`]s and drains
//! [`PersistResult`]s.
//!
//! All writes go through `oxtail_config::write_atomic` (via the config crate's
//! `save` functions), so pulling a USB stick cannot leave a torn file. With
//! an in-memory data folder nothing is written.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use oxtail_config::{
    DataDir, PathMapper, Profile, ProfileSet, Session, Settings, ThemeSet, write_atomic,
};

use crate::guistate::{GUI_STATE_FILE, GuiState};
use crate::startup::HISTORY_FILE;

/// Something to do on the persistence thread.
pub enum Job {
    /// Write `settings.toml`.
    Settings(Settings),
    /// Write `session.json`.
    Session(Session),
    /// Write a user profile into `profiles/`.
    Profile(Profile),
    /// Write the search history.
    History(Vec<String>),
    /// Write `gui-state.json` (column choices, panes, merged tabs).
    GuiState(Box<GuiState>),
    /// Reload `profiles/`.
    ReloadProfiles,
    /// Reload `themes/`.
    ReloadThemes,
    /// Reload `settings.toml`.
    ReloadSettings,
    /// Finish the queue and stop.
    Shutdown,
}

/// What the thread reports.
pub enum PersistResult {
    /// New profiles (and load warnings).
    Profiles(Arc<ProfileSet>, Vec<String>),
    /// New themes (and load warnings).
    Themes(Arc<ThemeSet>, Vec<String>),
    /// New settings (and a warning).
    Settings(Box<Settings>, Option<String>),
    /// A write finished: what and how it went.
    Saved(&'static str, Result<String, String>),
}

/// Handle to the persistence thread.
pub struct Persist {
    tx: Option<Sender<Job>>,
    finished: Receiver<()>,
    handle: Option<JoinHandle<()>>,
}

impl Persist {
    /// Starts the thread. `wake` is called after each result.
    pub fn spawn(
        data_dir: DataDir,
        mapper: PathMapper,
        results: Sender<PersistResult>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (tx, rx) = unbounded::<Job>();
        let (fin_tx, finished) = unbounded::<()>();
        if !data_dir.is_persistent() {
            return Self {
                tx: None,
                finished,
                handle: None,
            };
        }
        let handle = std::thread::Builder::new()
            .name("oxtail-persist".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if matches!(job, Job::Shutdown) {
                        break;
                    }
                    if let Some(r) = run_job(&data_dir, &mapper, job) {
                        let _ = results.send(r);
                        wake();
                    }
                }
                let _ = fin_tx.send(());
            })
            .map_err(|e| tracing::warn!("cannot start the persistence thread: {e}"))
            .ok();
        Self {
            tx: Some(tx),
            finished,
            handle,
        }
    }

    /// Whether writes go anywhere.
    pub fn is_active(&self) -> bool {
        self.tx.is_some()
    }

    /// Queues a job. Never blocks.
    pub fn send(&self, job: Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }

    /// Finishes the queued writes and waits at most `timeout` for them.
    ///
    /// Called once when the window closes, so the last session survives; it is
    /// the only place the GUI waits on the thread.
    pub fn shutdown(&mut self, timeout: Duration) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Job::Shutdown);
            let _ = self.finished.recv_timeout(timeout);
        }
        if self.handle.take().is_some() {
            // The thread has finished (or the timeout passed); it is not
            // joined so a stuck disk cannot hang the exit.
        }
    }
}

fn saved(
    what: &'static str,
    r: Result<std::path::PathBuf, oxtail_config::ConfigError>,
) -> PersistResult {
    PersistResult::Saved(
        what,
        r.map(|p| p.display().to_string())
            .map_err(|e| e.to_string()),
    )
}

fn run_job(dd: &DataDir, mapper: &PathMapper, job: Job) -> Option<PersistResult> {
    match job {
        Job::Settings(s) => {
            let path = dd.settings_path()?;
            Some(saved("settings", s.save(&path).map(|()| path)))
        }
        Job::Session(s) => {
            let path = dd.session_path()?;
            Some(saved("session", s.save(&path, mapper).map(|()| path)))
        }
        Job::Profile(p) => {
            let dir = dd.profiles_dir()?;
            Some(saved("profile", p.save_to_dir(&dir)))
        }
        Job::History(h) => {
            let path = dd.root.as_ref()?.join(HISTORY_FILE);
            let text = serde_json::to_vec(&h).ok()?;
            let r = write_atomic(&path, &text)
                .map(|()| path.clone())
                .map_err(|e| e.to_string());
            Some(PersistResult::Saved(
                "search history",
                r.map(|p| p.display().to_string()),
            ))
        }
        Job::GuiState(g) => {
            let path = dd.root.as_ref()?.join(GUI_STATE_FILE);
            let r = write_atomic(&path, &g.to_bytes())
                .map(|()| path.clone())
                .map_err(|e| e.to_string());
            Some(PersistResult::Saved(
                "GUI state",
                r.map(|p| p.display().to_string()),
            ))
        }
        Job::ReloadProfiles => {
            let dir = dd.profiles_dir()?;
            let l = ProfileSet::load_dir(&dir);
            Some(PersistResult::Profiles(Arc::new(l.value), l.warnings))
        }
        Job::ReloadThemes => {
            let dir = dd.themes_dir()?;
            let l = ThemeSet::load_dir(&dir);
            Some(PersistResult::Themes(Arc::new(l.value), l.warnings))
        }
        Job::ReloadSettings => {
            let path = dd.settings_path()?;
            let (s, w) = Settings::load(&path, &dd.mode);
            Some(PersistResult::Settings(Box::new(s), w))
        }
        Job::Shutdown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_config::DataMode;
    use std::time::Instant;

    fn dd(dir: &std::path::Path) -> DataDir {
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        DataDir {
            root: Some(dir.to_path_buf()),
            mode: DataMode::Cli,
        }
    }

    fn recv(rx: &Receiver<PersistResult>) -> PersistResult {
        rx.recv_timeout(Duration::from_secs(10)).expect("result")
    }

    #[test]
    fn saves_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let data = dd(tmp.path());
        let (tx, rx) = unbounded();
        let mut p = Persist::spawn(
            data.clone(),
            PathMapper::absolute_only(),
            tx,
            Arc::new(|| {}),
        );
        assert!(p.is_active());

        let settings = Settings {
            font_size: 17.0,
            ..Settings::default()
        };
        p.send(Job::Settings(settings.clone()));
        assert!(matches!(recv(&rx), PersistResult::Saved("settings", Ok(_))));
        p.send(Job::ReloadSettings);
        match recv(&rx) {
            PersistResult::Settings(s, w) => {
                assert!(w.is_none());
                assert_eq!(s.font_size, 17.0);
            }
            _ => panic!("expected settings"),
        }

        let profile = Profile {
            name: "Mine".into(),
            rules: vec!["match = { literal = \"x\" }".parse().unwrap()],
            ..Profile::default()
        };
        p.send(Job::Profile(profile));
        assert!(matches!(recv(&rx), PersistResult::Saved("profile", Ok(_))));
        p.send(Job::ReloadProfiles);
        match recv(&rx) {
            PersistResult::Profiles(set, _) => assert!(set.by_name("Mine").is_some()),
            _ => panic!("expected profiles"),
        }

        let mut session = Session::new();
        session.recent_files.push("/tmp/a.log".into());
        p.send(Job::Session(session));
        assert!(matches!(recv(&rx), PersistResult::Saved("session", Ok(_))));
        p.send(Job::History(vec!["q".into()]));
        assert!(matches!(
            recv(&rx),
            PersistResult::Saved("search history", Ok(_))
        ));
        let mut gs = GuiState::default();
        gs.set_structure(
            std::path::Path::new("/tmp/a.log"),
            crate::guistate::SavedStructure {
                table: true,
                ..Default::default()
            },
        );
        p.send(Job::GuiState(Box::new(gs.clone())));
        assert!(matches!(
            recv(&rx),
            PersistResult::Saved("GUI state", Ok(_))
        ));
        let bytes = std::fs::read(tmp.path().join(GUI_STATE_FILE)).unwrap();
        assert_eq!(GuiState::parse(&bytes).structures, gs.structures);
        p.send(Job::ReloadThemes);
        assert!(matches!(recv(&rx), PersistResult::Themes(..)));

        p.shutdown(Duration::from_secs(10));
        let loaded = Session::load(
            &tmp.path().join("session.json"),
            &PathMapper::absolute_only(),
        );
        assert_eq!(
            loaded.recent_files,
            vec![std::path::PathBuf::from("/tmp/a.log")]
        );
        let hist = std::fs::read(tmp.path().join(HISTORY_FILE)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Vec<String>>(&hist).unwrap(),
            vec!["q"]
        );
    }

    #[test]
    fn shutdown_flushes_queued_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = unbounded();
        let mut p = Persist::spawn(
            dd(tmp.path()),
            PathMapper::absolute_only(),
            tx,
            Arc::new(|| {}),
        );
        let mut s = Session::new();
        s.active_tab = 0;
        s.recent_files.push("/x".into());
        p.send(Job::Session(s));
        let start = Instant::now();
        p.shutdown(Duration::from_secs(10));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(tmp.path().join("session.json").exists());
    }

    #[test]
    fn in_memory_data_dir_writes_nothing() {
        let (tx, rx) = unbounded();
        let mut p = Persist::spawn(
            DataDir::in_memory("test"),
            PathMapper::absolute_only(),
            tx,
            Arc::new(|| {}),
        );
        assert!(!p.is_active());
        p.send(Job::Settings(Settings::default()));
        p.shutdown(Duration::from_millis(50));
        assert!(rx.try_recv().is_err());
    }
}
