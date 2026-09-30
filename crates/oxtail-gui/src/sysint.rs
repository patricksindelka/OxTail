//! System integration and the update check, run off the UI thread.
//!
//! Both are blocking (file system, registry, network), so every call runs on
//! a short-lived worker thread and reports back through a channel that the
//! frame drains with `try_recv` ([`SysState::poll`]). The real functions live
//! in `oxtail_config::{integration, update}`; [`SystemBackend`] is the seam
//! that lets tests answer instantly and offline.
//!
//! The update check never downloads or replaces anything: a newer release
//! becomes a dismissible notice with a link to its page.

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use oxtail_config::integration::IntegrationState;
use oxtail_config::update::Release;
use oxtail_config::{ConfigError, DataDir, integration, update};

/// What the shell needs from the system: registration and the update check.
/// Every method may block; call them from a worker thread only.
pub trait SystemBackend: Send + Sync {
    /// Whether OxTail is registered with the system.
    fn integration_state(&self, dd: &DataDir) -> IntegrationState;
    /// Registers OxTail; returns what was done.
    fn integrate(&self, dd: &DataDir, exe: &Path) -> Result<Vec<String>, ConfigError>;
    /// Removes the registration; returns what was undone.
    fn remove(&self, dd: &DataDir) -> Result<Vec<String>, ConfigError>;
    /// The newest release if it is newer than `current`.
    fn check_latest(&self, current: &str) -> Result<Option<Release>, ConfigError>;
    /// Whether an automatic check is due (throttling).
    fn check_due(&self, dd: &DataDir) -> bool;
    /// Records that a check happened.
    fn record_check(&self, dd: &DataDir) -> Result<(), ConfigError>;
}

/// The real thing: `oxtail_config::integration` and `oxtail_config::update`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConfigBackend;

impl SystemBackend for ConfigBackend {
    fn integration_state(&self, dd: &DataDir) -> IntegrationState {
        integration::state(dd)
    }
    fn integrate(&self, dd: &DataDir, exe: &Path) -> Result<Vec<String>, ConfigError> {
        integration::integrate(dd, exe)
    }
    fn remove(&self, dd: &DataDir) -> Result<Vec<String>, ConfigError> {
        integration::remove(dd)
    }
    fn check_latest(&self, current: &str) -> Result<Option<Release>, ConfigError> {
        update::check_latest(current)
    }
    fn check_due(&self, dd: &DataDir) -> bool {
        // A development build is never compared with releases (it would
        // always be older), so its automatic check never runs.
        !update::is_dev_build(env!("CARGO_PKG_VERSION")) && update::check_due(dd)
    }
    fn record_check(&self, dd: &DataDir) -> Result<(), ConfigError> {
        update::record_check(dd)
    }
}

/// What the integration section shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IntegrationView {
    /// Not asked yet.
    #[default]
    Unknown,
    /// A worker is looking.
    Checking,
    /// The answer.
    Known(IntegrationState),
}

/// How the last update check went.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateStatus {
    /// No check yet in this session.
    #[default]
    Idle,
    /// A worker is asking.
    Checking,
    /// Running the newest version.
    UpToDate,
    /// A newer release exists.
    Available(Release),
    /// The check failed (the text is user presentable).
    Failed(String),
}

/// What an announcement is about (the settings window shows the outcome of
/// its own topic itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// System integration (the System section).
    Integration,
    /// The update check (the Updates section).
    Update,
}

/// Results from the workers.
enum SysMsg {
    Integration(u64, IntegrationState),
    IntegrationDone {
        remove: bool,
        result: Result<Vec<String>, String>,
    },
    Update {
        manual: bool,
        result: Result<Option<Release>, String>,
    },
}

/// State of integration and updates, plus the channel from the workers.
pub struct SysState {
    backend: Arc<dyn SystemBackend>,
    tx: Sender<SysMsg>,
    rx: Receiver<SysMsg>,
    /// What the System section shows.
    pub integration: IntegrationView,
    /// A running integrate/remove job.
    pub integration_busy: bool,
    /// What the last integrate/remove did (or why it failed).
    pub integration_report: Option<Result<Vec<String>, String>>,
    /// How the update check went.
    pub update: UpdateStatus,
    /// The newer release to tell the user about (until dismissed).
    pub notice: Option<Release>,
    /// Outcomes worth telling the user about outside the settings window:
    /// `(text, is_error)`. The application drains this every frame.
    pub announcements: Vec<(String, bool, Topic)>,
    auto_started: bool,
    /// A manual check was asked for while an automatic one was running.
    manual_requested: bool,
    /// Generation of the integration state: bumped by every query and every
    /// change, so an answer that was overtaken is dropped.
    integration_gen: u64,
}

impl Default for SysState {
    fn default() -> Self {
        Self::new(Arc::new(ConfigBackend))
    }
}

impl SysState {
    /// State that talks to `backend`.
    pub fn new(backend: Arc<dyn SystemBackend>) -> Self {
        let (tx, rx) = unbounded();
        Self {
            backend,
            tx,
            rx,
            integration: IntegrationView::Unknown,
            integration_busy: false,
            integration_report: None,
            update: UpdateStatus::Idle,
            notice: None,
            announcements: Vec::new(),
            auto_started: false,
            manual_requested: false,
            integration_gen: 0,
        }
    }

    #[cfg(test)]
    fn manual_requested_done(&self) -> bool {
        !self.manual_requested
    }

    /// Whether a check is running.
    pub fn is_checking(&self) -> bool {
        self.update == UpdateStatus::Checking
    }

    /// Replaces the backend (tests).
    pub fn set_backend(&mut self, backend: Arc<dyn SystemBackend>) {
        self.backend = backend;
    }

    fn spawn(name: &str, job: impl FnOnce() + Send + 'static) -> bool {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .map_err(|e| tracing::warn!("cannot start the {name} thread: {e}"))
            .is_ok()
    }

    /// Asks the system whether OxTail is integrated (once, unless `force`).
    pub fn query_integration(
        &mut self,
        dd: &DataDir,
        wake: Arc<dyn Fn() + Send + Sync>,
        force: bool,
    ) {
        if !force && self.integration != IntegrationView::Unknown {
            return;
        }
        self.integration = IntegrationView::Checking;
        self.integration_gen += 1;
        let generation = self.integration_gen;
        let (backend, dd, tx) = (Arc::clone(&self.backend), dd.clone(), self.tx.clone());
        if !Self::spawn("oxtail-integration-state", move || {
            let _ = tx.send(SysMsg::Integration(
                generation,
                backend.integration_state(&dd),
            ));
            wake();
        }) {
            self.integration = IntegrationView::Known(IntegrationState::Unsupported(
                "cannot start a worker thread".into(),
            ));
        }
    }

    /// Registers (or with `remove`, unregisters) OxTail on a worker.
    pub fn change_integration(
        &mut self,
        dd: &DataDir,
        remove: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        if self.integration_busy {
            return;
        }
        self.integration_busy = true;
        self.integration_report = None;
        // Any query still running answers for the state before this change.
        self.integration_gen += 1;
        let (backend, dd, tx) = (Arc::clone(&self.backend), dd.clone(), self.tx.clone());
        if !Self::spawn("oxtail-integration", move || {
            let result = if remove {
                backend.remove(&dd)
            } else {
                match std::env::current_exe() {
                    Ok(exe) => backend.integrate(&dd, &exe),
                    Err(e) => Err(ConfigError::Integration(format!(
                        "cannot find the OxTail executable: {e}"
                    ))),
                }
            };
            let _ = tx.send(SysMsg::IntegrationDone {
                remove,
                result: result.map_err(|e| e.to_string()),
            });
            wake();
        }) {
            self.integration_busy = false;
            self.integration_report = Some(Err("cannot start a worker thread".into()));
        }
    }

    /// Checks for a newer release now (the "Check now" button and the
    /// palette command).
    pub fn check_now(&mut self, dd: &DataDir, wake: Arc<dyn Fn() + Send + Sync>) {
        self.start_check(dd, wake, true);
    }

    /// The automatic check at startup: only when `enabled` and the throttle
    /// says it is due; at most once per session.
    pub fn auto_check(&mut self, enabled: bool, dd: &DataDir, wake: Arc<dyn Fn() + Send + Sync>) {
        if !enabled || self.auto_started || !dd.is_persistent() {
            return;
        }
        self.auto_started = true;
        self.start_check(dd, wake, false);
    }

    fn start_check(&mut self, dd: &DataDir, wake: Arc<dyn Fn() + Send + Sync>, manual: bool) {
        if self.update == UpdateStatus::Checking {
            // Do not drop the request: an automatic check that is running
            // may not have asked the network, so `poll` follows up.
            self.manual_requested |= manual;
            return;
        }
        self.update = UpdateStatus::Checking;
        let (backend, dd, tx) = (Arc::clone(&self.backend), dd.clone(), self.tx.clone());
        if !Self::spawn("oxtail-update-check", move || {
            if !manual && !backend.check_due(&dd) {
                let _ = tx.send(SysMsg::Update {
                    manual,
                    result: Ok(None),
                });
                wake();
                return;
            }
            let result = backend
                .check_latest(env!("CARGO_PKG_VERSION"))
                .map_err(|e| e.to_string());
            if let Err(e) = backend.record_check(&dd) {
                tracing::debug!("cannot record the update check: {e}");
            }
            let _ = tx.send(SysMsg::Update { manual, result });
            wake();
        }) {
            self.update = UpdateStatus::Failed("cannot start a worker thread".into());
        }
    }

    /// Applies finished jobs. Never blocks. Returns whether anything changed.
    pub fn poll(&mut self, dd: &DataDir, wake: &Arc<dyn Fn() + Send + Sync>) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                SysMsg::Integration(generation, state) => {
                    if generation == self.integration_gen {
                        self.integration = IntegrationView::Known(state);
                    }
                }
                SysMsg::IntegrationDone { remove, result } => {
                    self.integration_busy = false;
                    let (text, error) = match &result {
                        Ok(items) if remove => (
                            format!("System integration removed ({} items)", items.len()),
                            false,
                        ),
                        Ok(items) => (
                            format!("Integrated with the system ({} items)", items.len()),
                            false,
                        ),
                        Err(e) => (format!("System integration failed: {e}"), true),
                    };
                    self.announcements.push((text, error, Topic::Integration));
                    self.integration_report = Some(result);
                    // What is registered now: ask again.
                    self.query_integration(dd, Arc::clone(wake), true);
                }
                SysMsg::Update { manual, result } => {
                    let requested = std::mem::take(&mut self.manual_requested);
                    if requested && !manual && matches!(result, Ok(None)) {
                        // The automatic check did not reach the network (not
                        // due, or nothing new): run the check that was asked for.
                        self.update = UpdateStatus::Idle;
                        self.start_check(dd, Arc::clone(wake), true);
                        continue;
                    }
                    let manual = manual || requested;
                    match result {
                        Ok(Some(release)) => {
                            self.notice = Some(release.clone());
                            self.update = UpdateStatus::Available(release);
                        }
                        Ok(None) => {
                            // A skipped automatic check says nothing.
                            if manual {
                                self.announcements.push((
                                    format!("OxTail {} is up to date", env!("CARGO_PKG_VERSION")),
                                    false,
                                    Topic::Update,
                                ));
                            }
                            self.update = if manual {
                                UpdateStatus::UpToDate
                            } else {
                                UpdateStatus::Idle
                            };
                        }
                        Err(e) => {
                            // Automatic checks fail quietly (offline is normal).
                            self.update = if manual {
                                self.announcements.push((
                                    format!("Update check failed: {e}"),
                                    true,
                                    Topic::Update,
                                ));
                                UpdateStatus::Failed(e)
                            } else {
                                tracing::debug!("update check failed: {e}");
                                UpdateStatus::Idle
                            };
                        }
                    }
                }
            }
        }
        changed
    }
}

/// Whether `url` may be opened in a browser: web pages only.
pub fn is_web_url(url: &str) -> bool {
    let u = url.trim();
    (u.starts_with("https://") || u.starts_with("http://")) && !u.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    #[derive(Default)]
    pub(crate) struct Fake {
        pub integrated: Mutex<bool>,
        pub release: Mutex<Option<Release>>,
        pub due: Mutex<bool>,
        pub checks: Mutex<u32>,
        pub fail: Mutex<bool>,
    }

    impl SystemBackend for Fake {
        fn integration_state(&self, _: &DataDir) -> IntegrationState {
            if *self.integrated.lock().unwrap() {
                IntegrationState::Integrated {
                    items: vec!["Start menu entry".into()],
                }
            } else {
                IntegrationState::NotIntegrated
            }
        }
        fn integrate(&self, _: &DataDir, _: &Path) -> Result<Vec<String>, ConfigError> {
            *self.integrated.lock().unwrap() = true;
            Ok(vec!["Created a Start menu entry".into()])
        }
        fn remove(&self, _: &DataDir) -> Result<Vec<String>, ConfigError> {
            *self.integrated.lock().unwrap() = false;
            Ok(vec!["Removed the Start menu entry".into()])
        }
        fn check_latest(&self, _: &str) -> Result<Option<Release>, ConfigError> {
            *self.checks.lock().unwrap() += 1;
            if *self.fail.lock().unwrap() {
                return Err(ConfigError::Update("offline".into()));
            }
            Ok(self.release.lock().unwrap().clone())
        }
        fn check_due(&self, _: &DataDir) -> bool {
            *self.due.lock().unwrap()
        }
        fn record_check(&self, _: &DataDir) -> Result<(), ConfigError> {
            Ok(())
        }
    }

    fn wake() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn dd() -> (tempfile::TempDir, DataDir) {
        let t = tempfile::tempdir().unwrap();
        let d = DataDir {
            root: Some(t.path().to_path_buf()),
            mode: oxtail_config::DataMode::Cli,
        };
        (t, d)
    }

    fn wait(s: &mut SysState, d: &DataDir, cond: impl Fn(&SysState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            s.poll(d, &wake());
            if cond(s) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release() -> Release {
        Release {
            version: "9.9.9".into(),
            url: "https://example.org/r".into(),
            notes: None,
        }
    }

    #[test]
    fn integration_state_and_changes_run_on_workers() {
        let (_t, d) = dd();
        let fake = Arc::new(Fake::default());
        let mut s = SysState::new(fake.clone());
        s.query_integration(&d, wake(), false);
        assert_eq!(s.integration, IntegrationView::Checking);
        wait(&mut s, &d, |s| {
            s.integration == IntegrationView::Known(IntegrationState::NotIntegrated)
        });
        s.change_integration(&d, false, wake());
        assert!(s.integration_busy);
        wait(&mut s, &d, |s| {
            !s.integration_busy
                && matches!(
                    s.integration,
                    IntegrationView::Known(IntegrationState::Integrated { .. })
                )
        });
        assert!(matches!(s.integration_report, Some(Ok(ref v)) if v.len() == 1));
        s.change_integration(&d, true, wake());
        wait(&mut s, &d, |s| {
            !s.integration_busy
                && s.integration == IntegrationView::Known(IntegrationState::NotIntegrated)
        });
    }

    #[test]
    fn the_automatic_check_needs_the_setting_the_throttle_and_a_data_folder() {
        let (_t, d) = dd();
        let fake = Arc::new(Fake::default());
        *fake.release.lock().unwrap() = Some(release());
        // Setting off: nothing happens.
        let mut s = SysState::new(fake.clone());
        s.auto_check(false, &d, wake());
        assert_eq!(s.update, UpdateStatus::Idle);
        // In-memory data folder: nothing happens.
        s.auto_check(true, &DataDir::in_memory("t"), wake());
        assert_eq!(s.update, UpdateStatus::Idle);
        // Not due: the worker asks the throttle and does not touch the network.
        s.auto_check(true, &d, wake());
        wait(&mut s, &d, |s| s.update != UpdateStatus::Checking);
        assert_eq!(*fake.checks.lock().unwrap(), 0);
        assert!(s.notice.is_none());
        // Due: a newer release becomes a notice.
        let mut s = SysState::new(fake.clone());
        *fake.due.lock().unwrap() = true;
        s.auto_check(true, &d, wake());
        wait(&mut s, &d, |s| s.notice.is_some());
        assert_eq!(*fake.checks.lock().unwrap(), 1);
        assert_eq!(s.update, UpdateStatus::Available(release()));
        // Once per session.
        s.notice = None;
        s.update = UpdateStatus::Idle;
        s.auto_check(true, &d, wake());
        assert_eq!(s.update, UpdateStatus::Idle);
    }

    #[test]
    fn a_manual_check_reports_every_outcome() {
        let (_t, d) = dd();
        let fake = Arc::new(Fake::default());
        let mut s = SysState::new(fake.clone());
        s.check_now(&d, wake());
        assert_eq!(s.update, UpdateStatus::Checking);
        wait(&mut s, &d, |s| s.update != UpdateStatus::Checking);
        assert_eq!(s.update, UpdateStatus::UpToDate);
        *fake.fail.lock().unwrap() = true;
        s.check_now(&d, wake());
        wait(&mut s, &d, |s| s.update != UpdateStatus::Checking);
        assert!(matches!(&s.update, UpdateStatus::Failed(m) if m.contains("offline")));
        *fake.fail.lock().unwrap() = false;
        *fake.release.lock().unwrap() = Some(release());
        s.check_now(&d, wake());
        wait(&mut s, &d, |s| s.notice.is_some());
        assert!(matches!(s.update, UpdateStatus::Available(_)));
    }

    #[test]
    fn a_manual_check_asked_during_an_automatic_one_is_not_dropped() {
        let (_t, d) = dd();
        let fake = Arc::new(Fake::default());
        // The automatic check is not due, so it never asks the network.
        let mut s = SysState::new(fake.clone());
        s.auto_check(true, &d, wake());
        s.check_now(&d, wake());
        assert!(s.manual_requested);
        wait(&mut s, &d, |s| {
            s.update == UpdateStatus::UpToDate && s.manual_requested_done()
        });
        assert_eq!(*fake.checks.lock().unwrap(), 1);
        assert!(
            s.announcements
                .iter()
                .any(|(t, _, topic)| t.contains("up to date") && *topic == Topic::Update)
        );
    }

    #[test]
    fn a_stale_integration_answer_is_dropped() {
        let (_t, d) = dd();
        let fake = Arc::new(Fake::default());
        let mut s = SysState::new(fake.clone());
        s.query_integration(&d, wake(), false);
        let stale = s.integration_gen;
        s.change_integration(&d, false, wake());
        wait(&mut s, &d, |s| {
            !s.integration_busy
                && matches!(
                    s.integration,
                    IntegrationView::Known(IntegrationState::Integrated { .. })
                )
        });
        // An answer from before the change arrives late.
        s.tx.send(SysMsg::Integration(stale, IntegrationState::NotIntegrated))
            .unwrap();
        s.poll(&d, &wake());
        assert!(matches!(
            s.integration,
            IntegrationView::Known(IntegrationState::Integrated { .. })
        ));
    }

    #[test]
    fn only_web_links_are_opened() {
        assert!(is_web_url("https://github.com/x/y/releases/tag/v1"));
        assert!(is_web_url("http://example.org"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("https://a b"));
        assert!(!is_web_url(""));
    }
}
