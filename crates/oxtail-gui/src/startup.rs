//! Everything loaded before the window opens, and the plain data the CLI
//! hands to the application.
//!
//! [`load_startup`] reads files. It is called once from `main`, before the
//! event loop exists (so before there is a UI thread to block); afterwards the
//! application only reads and writes configuration on worker threads.

use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, unbounded};
use oxtail_config::{DataDir, PathMapper, ProfileSet, Session, Settings, ThemeSet};

use crate::request::OpenRequest;

/// Configuration loaded at startup.
#[derive(Clone)]
pub struct Startup {
    /// The resolved data folder.
    pub data_dir: DataDir,
    /// User settings (defaults when the file is missing or broken).
    pub settings: Settings,
    /// The restorable session (empty when there is none).
    pub session: Session,
    /// Built-in and user profiles.
    pub profiles: Arc<ProfileSet>,
    /// Built-in and user themes.
    pub themes: Arc<ThemeSet>,
    /// Saved search queries, most recent first.
    pub history: Vec<String>,
    /// Non-fatal problems met while loading (shown as a banner).
    pub warnings: Vec<String>,
    /// Converts paths for the session file.
    pub mapper: PathMapper,
}

/// File name of the search history inside the data folder.
pub const HISTORY_FILE: &str = "search-history.json";

/// Loads settings, session, profiles, themes and the search history.
/// Never fails: problems become defaults plus entries in
/// [`Startup::warnings`].
pub fn load_startup(data_dir: DataDir) -> Startup {
    let mut warnings = Vec::new();
    let mapper = if data_dir.is_persistent() {
        PathMapper::current()
    } else {
        PathMapper::absolute_only()
    };
    let settings = match data_dir.settings_path() {
        Some(p) => {
            let (s, w) = Settings::load(&p, &data_dir.mode);
            warnings.extend(w);
            s
        }
        None => Settings::defaults_for(&data_dir.mode),
    };
    let session = data_dir
        .session_path()
        .map(|p| Session::load(&p, &mapper))
        .unwrap_or_default();
    let profiles = match data_dir.profiles_dir() {
        Some(dir) => {
            let loaded = ProfileSet::load_dir(&dir);
            warnings.extend(loaded.warnings);
            loaded.value
        }
        None => ProfileSet::builtin(),
    };
    let themes = match data_dir.themes_dir() {
        Some(dir) => {
            let loaded = ThemeSet::load_dir(&dir);
            warnings.extend(loaded.warnings);
            loaded.value
        }
        None => ThemeSet::builtin(),
    };
    let history = data_dir
        .root
        .as_ref()
        .and_then(|r| std::fs::read(r.join(HISTORY_FILE)).ok())
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default();
    Startup {
        data_dir,
        settings,
        session,
        profiles: Arc::new(profiles),
        themes: Arc::new(themes),
        history,
        warnings,
        mapper,
    }
}

/// A channel through which other instances (via IPC) ask this one to open
/// files, plus a slot the UI fills with its context so the IPC thread can wake
/// the event loop.
#[derive(Clone)]
pub struct ExternalOpen {
    /// Incoming requests.
    pub rx: Receiver<OpenRequest>,
    /// Filled by the application with its `egui::Context`; the sender calls
    /// `request_repaint()` on it after queueing a request.
    pub ctx_slot: Arc<Mutex<Option<egui::Context>>>,
}

impl ExternalOpen {
    /// A channel that never delivers anything (single-instance handling off).
    pub fn disconnected() -> Self {
        let (_tx, rx) = unbounded();
        Self {
            rx,
            ctx_slot: Arc::new(Mutex::new(None)),
        }
    }
}

/// Everything [`crate::OxTailApp::new`] needs. Cloneable so the CLI can retry
/// with another renderer.
#[derive(Clone)]
pub struct AppInit {
    /// Loaded configuration.
    pub startup: Startup,
    /// Files from the command line (opened in addition to the session).
    pub request: OpenRequest,
    /// Requests from other instances.
    pub external: ExternalOpen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_startup_in_memory_gives_defaults() {
        let s = load_startup(DataDir::in_memory("test"));
        assert!(s.session.tabs.is_empty());
        assert!(s.history.is_empty());
        assert!(s.profiles.by_name("Generic").is_some());
        assert!(!s.themes.themes().is_empty());
    }

    #[test]
    fn load_startup_reads_history_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(HISTORY_FILE), br#"["a","b"]"#).unwrap();
        let dd = DataDir {
            root: Some(dir.path().to_path_buf()),
            mode: oxtail_config::DataMode::Cli,
        };
        assert_eq!(load_startup(dd.clone()).history, vec!["a", "b"]);
        std::fs::write(dir.path().join(HISTORY_FILE), b"not json").unwrap();
        assert!(load_startup(dd).history.is_empty());
    }
}
