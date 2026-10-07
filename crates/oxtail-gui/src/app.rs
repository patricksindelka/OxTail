//! The application: tabs, opening files, actions, configuration, session.
//!
//! [`OxTailApp`] is the `eframe::App`. Its `show` method draws one frame
//! (`crate::appui`); everything else here is state and logic that never
//! blocks: files open on worker threads, configuration is read and written
//! by [`crate::persist`], notifications are raised by [`crate::alerts`].

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Context, ViewportCommand};
use oxtail_config::{
    ConfigChanged, ConfigKind, ConfigWatcher, DataDir, PathMapper, Profile, ProfileSet, Session,
    Settings, ThemeSet, WindowGeometry,
};
use oxtail_core::{Document, EncodingChoice, OpenOptions, TextEncoding};

use crate::alerts::{AlertEvent, AlertJob, AlertWorker, DesktopNotifier};
use crate::colors::{Colors, select_theme};
use crate::docview::{BookmarkInfo, DocView, ViewInit};
use crate::gototime::{GotoJob, GotoMsg, GotoTimeDialog, parse_goto_time};
use crate::guistate::GuiState;
use crate::keymap::KeyMap;
use crate::panes::{PaneId, PaneTree};
use crate::persist::{Job, Persist, PersistResult};
use crate::request::OpenRequest;
use crate::ruleeditor::{EditorAction, RuleEditor};
use crate::rules::profile_with_rules;
use crate::startup::{AppInit, ExternalOpen};
use crate::tab::{StructureCtx, Tab, TabContent, TabInit, apply_sniffed_profile, build_session};

/// How often the session is saved when something changed.
pub const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(30);
/// How long a settings change waits before it is written.
pub const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(800);
/// Font size limits in points.
pub const MIN_FONT: f32 = 6.0;
/// Font size limits in points.
pub const MAX_FONT: f32 = 48.0;
/// Lines requested for the first screen when following.
const INITIAL_TAIL_LINES: usize = 120;

/// Messages from worker threads to the UI.
pub enum AppMsg {
    /// A document finished opening (or failed).
    Opened {
        /// The tab that asked for it.
        tab_id: u64,
        /// The document or a user-presentable error.
        result: Result<Arc<Document>, String>,
    },
    /// Files to open (chosen in the dialog, or found in dropped folders).
    Files(Vec<PathBuf>),
    /// The open dialog was closed.
    DialogDone,
    /// The config watcher is running.
    Watcher(ConfigWatcher, Receiver<ConfigChanged>),
    /// Which recent files still exist (checked off the UI thread).
    RecentStatus(Vec<(PathBuf, bool)>),
    /// The files of a merged tab were opened (or opening failed).
    MergeOpened {
        /// The tab that asked for it.
        tab_id: u64,
        /// The sources or a user-presentable error.
        result: Result<Vec<crate::merge::MergeSource>, String>,
    },
}

/// A message under the menu bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The text.
    pub text: String,
    /// Whether it is an error (otherwise informational).
    pub error: bool,
}

/// State of the "Go to line" dialog.
#[derive(Debug, Clone, Default)]
pub struct GotoDialog {
    /// The input.
    pub text: String,
    /// Why the input was rejected.
    pub error: Option<String>,
    /// Focus the text field on the next frame.
    pub focus: bool,
}

/// Which windows are open.
#[derive(Default)]
pub struct Windows {
    /// Go to line.
    pub goto: Option<GotoDialog>,
    /// Go to time.
    pub goto_time: Option<GotoTimeDialog>,
    /// Merge tabs.
    pub merge: Option<crate::appmerge::MergeDialog>,
    /// Search across the open tabs.
    pub cross: crate::cross::CrossSearchState,
    /// Settings.
    pub settings: bool,
    /// Keyboard shortcuts.
    pub shortcuts: bool,
    /// About.
    pub about: bool,
}

/// The OxTail application.
pub struct OxTailApp {
    pub(crate) ctx: Option<Context>,
    pub(crate) data_dir: DataDir,
    pub(crate) mapper: PathMapper,
    pub(crate) settings: Settings,
    pub(crate) session: Session,
    pub(crate) last_saved_session: Option<Session>,
    pub(crate) profiles: Arc<ProfileSet>,
    pub(crate) themes: Arc<ThemeSet>,
    pub(crate) colors: Colors,
    pub(crate) style_epoch: u64,
    last_system_dark: Option<bool>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    next_tab_id: u64,
    msg_tx: Sender<AppMsg>,
    msg_rx: Receiver<AppMsg>,
    /// Paces the repaints documents ask for while their tabs follow.
    pub(crate) pacer: Arc<crate::pacer::WakePacer>,
    persist_rx: Receiver<PersistResult>,
    persist_tx: Sender<PersistResult>,
    persist: Option<Persist>,
    alerts: Option<AlertWorker>,
    external: ExternalOpen,
    queue: VecDeque<OpenRequest>,
    restore: Vec<RestoreTab>,
    pub(crate) history: Vec<String>,
    pub(crate) windows: Windows,
    pub(crate) rule_editor: RuleEditor,
    pub(crate) notices: Vec<Notice>,
    config_rx: Option<Receiver<ConfigChanged>>,
    _config_watcher: Option<ConfigWatcher>,
    last_session_save: Instant,
    settings_dirty_at: Option<Instant>,
    pub(crate) history_dirty: bool,
    /// GUI-side state (column choices, pane layout, merged tabs).
    pub(crate) gui_state: GuiState,
    last_saved_gui_state: Option<GuiState>,
    /// The split layout of the tab area.
    pub(crate) panes: PaneTree,
    /// The pane that has the keyboard focus.
    pub(crate) focused_pane: PaneId,
    pub(crate) next_pane_id: PaneId,
    /// The active tab (by id) of each pane.
    pub(crate) pane_active: HashMap<PaneId, u64>,
    /// Panes showing the same file follow each other's cursor.
    pub(crate) sync_cursor: bool,
    pub(crate) sync_last: HashMap<u64, Option<u64>>,
    /// Where the panes were drawn last frame (for tab drops).
    pub(crate) pane_rects: Vec<(PaneId, egui::Rect)>,
    /// The tab being dragged.
    pub(crate) dragging_tab: Option<u64>,
    pub(crate) last_title: String,
    pub(crate) file_dialog_requested: bool,
    pub(crate) window: Option<WindowGeometry>,
    pub(crate) file_dialog_open: bool,
    /// The active key bindings (preset plus the user's overrides).
    pub(crate) keymap: KeyMap,
    /// The command palette.
    pub(crate) palette: crate::palette::PaletteState,
    /// The settings window's own state (section, key recorder).
    pub(crate) settings_ui: crate::settingsui::SettingsUi,
    /// System integration and the update check.
    pub(crate) sys: crate::sysint::SysState,
    /// Which recent files exist, as far as the last check knows.
    pub(crate) recent_exists: HashMap<PathBuf, bool>,
    recent_checked: Vec<PathBuf>,
    started: bool,
}

/// A restored tab waiting to be opened.
struct RestoreTab {
    path: PathBuf,
    init: TabInit,
    active: bool,
    pane: PaneId,
}

impl OxTailApp {
    /// Creates the application for eframe.
    pub fn new(cc: &eframe::CreationContext<'_>, init: AppInit) -> Self {
        let mut app = Self::from_init(init);
        app.ensure_started(&cc.egui_ctx);
        app
    }

    /// Creates the application state without a UI context. The context is
    /// bound on the first frame ([`OxTailApp::show`]) or by
    /// [`OxTailApp::ensure_started`].
    pub fn from_init(init: AppInit) -> Self {
        let AppInit {
            startup,
            request,
            external,
        } = init;
        let (msg_tx, msg_rx) = unbounded();
        let (persist_tx, persist_rx) = unbounded();
        let colors = Colors::from_theme(&select_theme(&startup.settings, &startup.themes, None));
        let mut notices: Vec<Notice> = startup
            .warnings
            .iter()
            .map(|w| Notice {
                text: w.clone(),
                error: true,
            })
            .collect();
        if let Some(reason) = startup.data_dir.in_memory_reason() {
            notices.push(Notice {
                text: format!("Settings will not be saved: {reason}"),
                error: false,
            });
        }
        let mut app = Self {
            ctx: None,
            data_dir: startup.data_dir,
            mapper: startup.mapper,
            settings: startup.settings,
            session: startup.session.clone(),
            last_saved_session: None,
            profiles: startup.profiles,
            themes: startup.themes,
            colors,
            style_epoch: 1,
            last_system_dark: None,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            msg_tx,
            msg_rx,
            pacer: crate::pacer::WakePacer::new(),
            persist_rx,
            persist_tx,
            persist: None,
            alerts: None,
            external,
            queue: VecDeque::new(),
            restore: Vec::new(),
            history: startup.history,
            windows: Windows::default(),
            rule_editor: RuleEditor::default(),
            notices,
            config_rx: None,
            _config_watcher: None,
            last_session_save: Instant::now(),
            settings_dirty_at: None,
            history_dirty: false,
            gui_state: startup.gui_state,
            last_saved_gui_state: None,
            panes: PaneTree::leaf(0),
            focused_pane: 0,
            next_pane_id: 1,
            pane_active: HashMap::new(),
            sync_cursor: false,
            sync_last: HashMap::new(),
            pane_rects: Vec::new(),
            dragging_tab: None,
            last_title: String::new(),
            file_dialog_requested: false,
            window: startup.session.window,
            file_dialog_open: false,
            keymap: KeyMap::default(),
            palette: crate::palette::PaletteState::default(),
            settings_ui: crate::settingsui::SettingsUi::default(),
            sys: crate::sysint::SysState::default(),
            recent_exists: HashMap::new(),
            recent_checked: Vec::new(),
            started: false,
        };
        app.rebuild_keymap();
        app.plan_restore(&startup.session);
        if !request.is_empty() {
            app.queue.push_back(request);
        }
        app
    }

    /// Binds the UI context and starts the workers, once.
    pub fn ensure_started(&mut self, ctx: &Context) {
        if self.started {
            return;
        }
        self.started = true;
        self.ctx = Some(ctx.clone());
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // A log viewer should feel instant: no fading menus, popups and
        // windows, no eased scrolling in scroll areas.
        ctx.all_styles_mut(|s| {
            s.animation_time = 0.0;
            s.scroll_animation = egui::style::ScrollAnimation::none();
        });
        if let Ok(mut slot) = self.external.ctx_slot.lock() {
            *slot = Some(ctx.clone());
        }
        let wake = self.waker();
        self.persist = Some(Persist::spawn(
            self.data_dir.clone(),
            self.mapper.clone(),
            self.persist_tx.clone(),
            Arc::clone(&wake),
        ));
        self.alerts = Some(AlertWorker::spawn(wake, Arc::new(DesktopNotifier)));
        self.start_config_watcher();
        self.apply_theme(ctx);
        let wake = self.waker();
        self.sys
            .auto_check(self.settings.update_check, &self.data_dir, wake);
        // Restored tabs first, then what was asked for on the command line.
        let restore = std::mem::take(&mut self.restore);
        for r in restore {
            self.open_path_in(r.path, r.init, r.active, r.pane);
        }
        self.restore_layout();
        for def in self.gui_state.merged.clone() {
            if def.paths.len() >= 2 {
                self.open_merged_paths(def.paths);
            }
        }
        let queued: Vec<OpenRequest> = self.queue.drain(..).collect();
        for req in queued {
            self.open_request(req);
        }
    }

    /// A sender for messages from worker threads to the UI.
    pub(crate) fn msg_sender(&self) -> Sender<AppMsg> {
        self.msg_tx.clone()
    }

    /// A callback that wakes the event loop (safe to call from any thread).
    pub(crate) fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        match &self.ctx {
            Some(ctx) => {
                let ctx = ctx.clone();
                Arc::new(move || ctx.request_repaint())
            }
            None => Arc::new(|| {}),
        }
    }

    /// The waker for documents: like [`Self::waker`], but paced while every
    /// visible view follows its end (see [`crate::pacer`]).
    pub(crate) fn doc_waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        match &self.ctx {
            Some(ctx) => {
                let ctx = ctx.clone();
                let pacer = Arc::clone(&self.pacer);
                Arc::new(move || pacer.wake(&ctx))
            }
            None => Arc::new(|| {}),
        }
    }

    /// Whether document repaints are paced (every visible view follows).
    pub fn repaints_paced(&self) -> bool {
        self.pacer.is_paced()
    }

    /// Paces document repaints only while every visible view follows. Runs
    /// before and after drawing: drawing can stop following (wheel, minimap,
    /// scrollbar), and a reply deferred meanwhile then needs a frame.
    pub(crate) fn update_pacing(&self, ctx: &Context) {
        let following = self.panes.panes().iter().all(|&p| {
            self.active_index_in_pane(p)
                .and_then(|i| self.tabs.get(i))
                .is_none_or(|t| match (t.view(), t.merged()) {
                    (Some(v), _) => v.follow,
                    (None, Some(m)) => m.follow,
                    (None, None) => true,
                })
        });
        if self.pacer.set_paced(following) {
            ctx.request_repaint_after(crate::pacer::ONE_FRAME);
        }
    }

    fn start_config_watcher(&mut self) {
        let Some(root) = self.data_dir.root.clone() else {
            return;
        };
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        // Creating the watcher touches the file system; do it off the UI thread.
        let spawned = std::thread::Builder::new()
            .name("oxtail-config-watch-init".into())
            .spawn(
                move || match ConfigWatcher::new(&root, Duration::from_millis(250)) {
                    Ok((w, rx)) => {
                        let _ = tx.send(AppMsg::Watcher(w, rx));
                        wake();
                    }
                    Err(e) => tracing::warn!("config hot reload unavailable: {e}"),
                },
            );
        if let Err(e) = spawned {
            tracing::warn!("cannot start the config watcher thread: {e}");
        }
    }

    // ------------------------------------------------------------ restoring

    fn plan_restore(&mut self, session: &Session) {
        let limit = 64;
        for (i, t) in session.tabs.iter().take(limit).enumerate() {
            let bookmarks = session.bookmarks.get(&t.path).cloned().unwrap_or_default();
            self.restore.push(RestoreTab {
                path: t.path.clone(),
                active: i == session.active_tab,
                pane: 0,
                init: TabInit {
                    follow: t.follow,
                    start_lines: None,
                    filters: t.filters.clone(),
                    profile: t.profile.clone(),
                    initial_line: (!t.follow).then_some(t.scroll_anchor_line),
                    bookmarks,
                    search: t.search_query.clone(),
                    encoding: t.encoding.clone(),
                    wrap: self.settings.wrap,
                },
            });
        }
    }

    // -------------------------------------------------------------- opening

    pub(crate) fn next_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    pub(crate) fn open_options(&self, init: &TabInit, stdin: bool) -> OpenOptions {
        let encoding = init
            .encoding
            .as_deref()
            .or(Some(self.settings.default_encoding.as_str()))
            .filter(|l| !l.eq_ignore_ascii_case("auto") && !l.is_empty())
            .and_then(|l| TextEncoding::from_label(l).ok())
            .map_or(EncodingChoice::Auto, EncodingChoice::Fixed);
        OpenOptions {
            encoding,
            spool_dir: self.data_dir.tmp_dir(),
            cache_bytes: (self.settings.cache_size_mb as usize).saturating_mul(1024 * 1024),
            max_display_len: self.settings.max_display_line_length,
            start_at_tail: if stdin || init.start_lines.is_some() || init.initial_line.is_some() {
                None
            } else {
                Some(INITIAL_TAIL_LINES)
            },
            ..OpenOptions::default()
        }
    }

    /// Handles a request to open files (command line, IPC, drop, dialog).
    pub fn open_request(&mut self, req: OpenRequest) {
        if req.is_empty() {
            return;
        }
        if !self.started {
            // Workers need the UI context to wake the event loop.
            self.queue.push_back(req);
            return;
        }
        if req.merge && req.files.len() >= 2 {
            self.open_merged_paths(req.files.clone());
            return;
        }
        for path in &req.files {
            let abs = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.path.as_deref() == Some(abs.as_path()))
            {
                self.select_tab(i);
                continue;
            }
            let init = TabInit {
                follow: req.tail_lines.is_none(),
                start_lines: req.tail_lines,
                filters: req.filter.iter().cloned().collect(),
                profile: req.profile.clone(),
                wrap: self.settings.wrap,
                ..TabInit::default()
            };
            self.open_path(abs, init, true);
        }
        if req.stdin {
            self.open_stdin(&req);
        }
    }

    /// Starts opening `path` in a new tab. The tab shows "Opening..." until the
    /// worker thread delivers the document.
    pub fn open_path(&mut self, path: PathBuf, init: TabInit, activate: bool) -> u64 {
        let pane = self.focused_pane;
        self.open_path_in(path, init, activate, pane)
    }

    /// Like [`OxTailApp::open_path`], in a given pane.
    pub fn open_path_in(
        &mut self,
        path: PathBuf,
        init: TabInit,
        activate: bool,
        pane: PaneId,
    ) -> u64 {
        let id = self.next_id();
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let opts = self.open_options(&init, false);
        let mut tab = Tab::opening(id, title, Some(path.clone()), init);
        tab.pane = pane;
        self.tabs.push(tab);
        self.pane_active.entry(pane).or_insert(id);
        if activate {
            self.select_tab(self.tabs.len() - 1);
        }
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let doc_wake = self.doc_waker();
        let spawned = std::thread::Builder::new()
            .name("oxtail-open".into())
            .spawn(move || {
                let result = Document::open(&path, opts)
                    .map(|d| {
                        let d = Arc::new(d);
                        let w = Arc::clone(&doc_wake);
                        d.set_waker(Box::new(move || w()));
                        d
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(AppMsg::Opened { tab_id: id, result });
                wake();
            });
        if let Err(e) = spawned {
            self.finish_open(id, Err(format!("cannot start a thread: {e}")));
        }
        id
    }

    fn open_stdin(&mut self, req: &OpenRequest) {
        let id = self.next_id();
        let init = TabInit {
            follow: true,
            filters: req.filter.iter().cloned().collect(),
            profile: req.profile.clone(),
            wrap: self.settings.wrap,
            ..TabInit::default()
        };
        let mut tab = Tab::opening(id, "<stdin>".into(), None, init);
        tab.pane = self.focused_pane;
        self.tabs.push(tab);
        self.select_tab(self.tabs.len() - 1);
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let doc_wake = self.doc_waker();
        let spool = self.data_dir.tmp_dir();
        let spawned = std::thread::Builder::new()
            .name("oxtail-open-stdin".into())
            .spawn(move || {
                let result = Document::from_stdin(spool.as_deref())
                    .map(|d| {
                        let d = Arc::new(d);
                        let w = Arc::clone(&doc_wake);
                        d.set_waker(Box::new(move || w()));
                        d
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(AppMsg::Opened { tab_id: id, result });
                wake();
            });
        if let Err(e) = spawned {
            self.finish_open(id, Err(format!("cannot start a thread: {e}")));
        }
    }

    /// Opens an already created document in a new tab (used by tests and
    /// embedders).
    pub fn open_document(&mut self, title: &str, doc: Arc<Document>) -> u64 {
        let id = self.next_id();
        let init = TabInit {
            follow: true,
            wrap: self.settings.wrap,
            ..TabInit::default()
        };
        let mut tab = Tab::opening(id, title.into(), None, init);
        tab.pane = self.focused_pane;
        self.tabs.push(tab);
        self.select_tab(self.tabs.len() - 1);
        let w = self.doc_waker();
        doc.set_waker(Box::new(move || w()));
        self.finish_open(id, Ok(doc));
        id
    }

    fn finish_open(&mut self, tab_id: u64, result: Result<Arc<Document>, String>) {
        let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let init = match &self.tabs[i].content {
            TabContent::Opening(init) => init.clone(),
            _ => return,
        };
        match result {
            Ok(doc) => {
                let mut view = DocView::new(
                    doc,
                    &ViewInit {
                        follow: init.follow,
                        wrap: init.wrap,
                        start_lines: init.start_lines,
                        initial_line: init.initial_line,
                    },
                );
                view.wake = self.waker();
                if !init.filters.is_empty() {
                    view.filter.set_from_query_strings(&init.filters);
                    view.filter.open = true;
                }
                if let Some(q) = &init.search
                    && !q.is_empty()
                {
                    view.find.text.clone_from(q);
                    view.find.open = true;
                    view.find.restart_now(Instant::now());
                }
                for b in &init.bookmarks {
                    view.bookmarks.insert(
                        b.line,
                        BookmarkInfo {
                            label: b.label.clone(),
                            offset: None,
                        },
                    );
                }
                if let Some(p) = init.profile.as_deref()
                    && let Some(profile) = self.profiles.by_name(p)
                {
                    view.hl.borrow_mut().set_profile(Some(profile));
                }
                view.request_sniff();
                self.tabs[i].content = TabContent::Ready(Box::new(view));
                if let Some(path) = self.tabs[i].path.clone() {
                    self.session
                        .add_recent(&path, self.settings.recent_files_limit.max(1));
                }
            }
            Err(msg) => {
                self.notices.push(Notice {
                    text: format!("Cannot open {}: {msg}", self.tabs[i].title),
                    error: true,
                });
                self.tabs[i].content = TabContent::Failed(msg);
            }
        }
    }

    /// Opens the file dialog on a worker thread.
    pub fn pick_files(&mut self) {
        if self.file_dialog_open {
            return;
        }
        self.file_dialog_open = true;
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let start_dir = self
            .session
            .recent_files
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf);
        let spawned = std::thread::Builder::new()
            .name("oxtail-file-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Open log file");
                if let Some(d) = start_dir {
                    dialog = dialog.set_directory(d);
                }
                let picked = pollster::block_on(dialog.pick_files());
                let paths: Vec<PathBuf> = picked
                    .unwrap_or_default()
                    .into_iter()
                    .map(|h| h.path().to_path_buf())
                    .collect();
                if !paths.is_empty() {
                    let _ = tx.send(AppMsg::Files(paths));
                }
                let _ = tx.send(AppMsg::DialogDone);
                wake();
            });
        if spawned.is_err() {
            self.file_dialog_open = false;
        }
    }

    /// Opens the folder dialog on a worker thread; the files directly inside
    /// the chosen folder open as tabs (see [`expand_dropped`]).
    pub fn pick_folder(&mut self) {
        if self.file_dialog_open {
            return;
        }
        self.file_dialog_open = true;
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let start_dir = self
            .session
            .recent_files
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf);
        let spawned = std::thread::Builder::new()
            .name("oxtail-folder-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Open folder");
                if let Some(d) = start_dir {
                    dialog = dialog.set_directory(d);
                }
                if let Some(h) = pollster::block_on(dialog.pick_folder()) {
                    let files = expand_dropped(&[h.path().to_path_buf()]);
                    if !files.is_empty() {
                        let _ = tx.send(AppMsg::Files(files));
                    }
                }
                let _ = tx.send(AppMsg::DialogDone);
                wake();
            });
        if spawned.is_err() {
            self.file_dialog_open = false;
        }
    }

    /// Opens `text` (for example from the clipboard) as a new tab named
    /// `title`. The document is built on a worker thread.
    pub fn open_text_tab(&mut self, title: &str, text: String) {
        let id = self.next_id();
        let init = TabInit {
            follow: false,
            initial_line: Some(0),
            wrap: self.settings.wrap,
            ..TabInit::default()
        };
        let mut tab = Tab::opening(id, title.to_string(), None, init);
        tab.pane = self.focused_pane;
        self.tabs.push(tab);
        self.select_tab(self.tabs.len() - 1);
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let name = title.to_string();
        let spawned = std::thread::Builder::new()
            .name("oxtail-open-text".into())
            .spawn(move || {
                let doc = Document::from_source(
                    Arc::new(oxtail_core::MemSource::new(text.into_bytes())),
                    name,
                );
                let doc = Arc::new(doc);
                let w = Arc::clone(&wake);
                doc.set_waker(Box::new(move || w()));
                let _ = tx.send(AppMsg::Opened {
                    tab_id: id,
                    result: Ok(doc),
                });
                wake();
            });
        if let Err(e) = spawned {
            self.finish_open(id, Err(format!("cannot start a thread: {e}")));
        }
    }

    /// Checks (on a worker) which recent files still exist, when the list
    /// changed. The start screen and the "Open recent" menu dim the missing.
    pub(crate) fn refresh_recent_status(&mut self) {
        if self.recent_checked == self.session.recent_files {
            return;
        }
        self.recent_checked.clone_from(&self.session.recent_files);
        let files = self.session.recent_files.clone();
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let spawned = std::thread::Builder::new()
            .name("oxtail-recent-check".into())
            .spawn(move || {
                let status: Vec<(PathBuf, bool)> = files
                    .into_iter()
                    .map(|p| {
                        let ok = std::fs::metadata(&p).is_ok();
                        (p, ok)
                    })
                    .collect();
                let _ = tx.send(AppMsg::RecentStatus(status));
                wake();
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the recent files check: {e}");
        }
    }

    // ----------------------------------------------------------------- tabs

    /// Makes tab `i` the active one (and its pane the focused one).
    pub fn select_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
            self.tabs[i].badge = 0;
            let (id, pane) = (self.tabs[i].id, self.tabs[i].pane);
            self.pane_active.insert(pane, id);
            self.focused_pane = pane;
        }
    }

    /// Closes tab `i`.
    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let closed_id = self.tabs[i].id;
        let pane = self.tabs[i].pane;
        let was_active_in_pane = self.pane_active.get(&pane) == Some(&closed_id);
        let tab = self.tabs.remove(i);
        self.sync_last.remove(&closed_id);
        match tab.content {
            TabContent::Ready(view) => {
                // Merged tabs that share this document now have to drain its
                // events themselves.
                for t in &mut self.tabs {
                    if let Some(m) = t.merged_mut() {
                        m.adopt(&view.doc);
                    }
                }
                // Dropping a Document stops its actor thread, which may wait a
                // few milliseconds; do that off the UI thread.
                let doc = Arc::clone(&view.doc);
                drop(view);
                let _ = std::thread::Builder::new()
                    .name("oxtail-close".into())
                    .spawn(move || drop(doc));
            }
            TabContent::Merged(m) => {
                let _ = std::thread::Builder::new()
                    .name("oxtail-close".into())
                    .spawn(move || drop(m));
            }
            _ => {}
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if i < self.active {
            self.active -= 1;
        }
        // The pane may be empty now, or have lost its active tab.
        let remaining = self.tabs_in_pane(pane);
        if remaining.is_empty() {
            self.remove_pane(pane);
        } else if was_active_in_pane {
            // The tab that took its place, else the last one.
            let next = remaining
                .iter()
                .copied()
                .find(|&r| r >= i)
                .unwrap_or(remaining[remaining.len() - 1]);
            self.pane_active.insert(pane, self.tabs[next].id);
        }
        self.sync_active();
    }

    /// The active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// The active tab's view, if its document is open.
    pub fn active_view(&self) -> Option<&DocView> {
        self.active_tab().and_then(Tab::view)
    }

    /// The active tab's view, mutably.
    pub fn active_view_mut(&mut self) -> Option<&mut DocView> {
        self.tabs.get_mut(self.active).and_then(Tab::view_mut)
    }

    /// Number of tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// The current settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether the watcher for hand-edited config files has started (it is
    /// created in the background after launch).
    pub fn config_watcher_running(&self) -> bool {
        self._config_watcher.is_some()
    }

    /// Names of the available profiles (user profiles first).
    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.profiles().map(|p| p.name.clone()).collect()
    }

    // ------------------------------------------------------- frame plumbing

    /// Everything that happens before drawing: messages, events, keys.
    pub(crate) fn pre_frame(&mut self, ctx: &Context) {
        let now = Instant::now();
        self.drain_messages(ctx);
        self.drain_persist(ctx);
        self.poll_system(ctx);
        self.refresh_recent_status();
        self.poll_goto_time(ctx);
        if self.windows.cross.poll() {
            ctx.request_repaint();
        }
        if self.windows.cross.running {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.drain_external(ctx);
        self.handle_dropped_files(ctx);
        self.drain_config_changes();
        self.pump_tabs(ctx, now);
        self.sync_cursors();
        self.drain_alerts();
        self.track_window(ctx);
        self.follow_system_theme(ctx);
        self.handle_zoom(ctx);
        self.handle_keys(ctx);
        self.periodic(ctx, now);
        self.update_pacing(ctx);
    }

    fn drain_messages(&mut self, ctx: &Context) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                AppMsg::Opened { tab_id, result } => self.finish_open(tab_id, result),
                AppMsg::Files(paths) => {
                    self.open_request(OpenRequest {
                        files: paths,
                        ..OpenRequest::default()
                    });
                }
                AppMsg::DialogDone => self.file_dialog_open = false,
                AppMsg::RecentStatus(list) => {
                    self.recent_exists = list.into_iter().collect();
                }
                AppMsg::MergeOpened { tab_id, result } => self.finish_merge_open(tab_id, result),
                AppMsg::Watcher(w, rx) => {
                    self._config_watcher = Some(w);
                    self.config_rx = Some(rx);
                }
            }
            ctx.request_repaint();
        }
    }

    fn drain_persist(&mut self, ctx: &Context) {
        while let Ok(r) = self.persist_rx.try_recv() {
            match r {
                PersistResult::Profiles(set, warnings) => {
                    self.profiles = set;
                    self.notify_warnings(warnings);
                    self.reapply_profiles();
                }
                PersistResult::Themes(set, warnings) => {
                    self.themes = set;
                    self.notify_warnings(warnings);
                    self.apply_theme(ctx);
                }
                PersistResult::Settings(s, warning) => {
                    if let Some(w) = warning {
                        self.notify_warnings(vec![w]);
                    }
                    if *s != self.settings {
                        let keys_changed = s.keymap != self.settings.keymap
                            || s.custom_keybindings != self.settings.custom_keybindings;
                        self.settings = *s;
                        if keys_changed {
                            self.rebuild_keymap();
                        }
                        self.apply_theme(ctx);
                        self.bump_style();
                    }
                }
                PersistResult::Saved(what, Err(e)) => self.notices.push(Notice {
                    text: format!("Could not save {what}: {e}"),
                    error: true,
                }),
                PersistResult::Saved(what, Ok(path)) => {
                    tracing::debug!("saved {what} to {path}");
                    if what == "profile" {
                        self.rule_editor.status = Some(format!("Saved to {path}"));
                    }
                }
            }
            ctx.request_repaint();
        }
    }

    fn notify_warnings(&mut self, warnings: Vec<String>) {
        for w in warnings {
            let n = Notice {
                text: w,
                error: true,
            };
            if !self.notices.contains(&n) {
                self.notices.push(n);
            }
        }
    }

    fn reapply_profiles(&mut self) {
        let profiles = Arc::clone(&self.profiles);
        for tab in &mut self.tabs {
            if tab.customized {
                continue;
            }
            // The profile the user chose wins over whatever is showing now.
            let name = tab
                .forced_profile
                .clone()
                .or_else(|| tab.view().and_then(|v| v.hl.borrow().profile.clone()));
            let Some(view) = tab.view_mut() else { continue };
            if let Some(name) = name
                && let Some(p) = profiles.by_name(&name)
            {
                view.hl.borrow_mut().set_profile(Some(p));
            }
        }
    }

    fn drain_external(&mut self, ctx: &Context) {
        let mut any = false;
        while let Ok(req) = self.external.rx.try_recv() {
            any = true;
            self.open_request(req);
        }
        if any {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        // A dropped folder opens the files inside it. Finding out what is a
        // folder touches the file system, so a worker does it.
        let tx = self.msg_tx.clone();
        let wake = self.waker();
        let spawned = std::thread::Builder::new()
            .name("oxtail-drop".into())
            .spawn(move || {
                let files = expand_dropped(&dropped);
                if !files.is_empty() {
                    let _ = tx.send(AppMsg::Files(files));
                    wake();
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the drop thread: {e}");
        }
    }

    fn drain_config_changes(&mut self) {
        let mut kinds = Vec::new();
        if let Some(rx) = &self.config_rx {
            while let Ok(ConfigChanged(kind)) = rx.try_recv() {
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
        }
        for kind in kinds {
            let job = match kind {
                ConfigKind::Settings => Job::ReloadSettings,
                ConfigKind::Profiles => Job::ReloadProfiles,
                ConfigKind::Themes => Job::ReloadThemes,
            };
            self.send_persist(job);
        }
    }

    fn pump_tabs(&mut self, ctx: &Context, now: Instant) {
        let profiles = Arc::clone(&self.profiles);
        let notify = self.settings.notifications_enabled;
        let mut copied: Option<String> = None;
        let tz = self.settings.timezone.clone();
        let sort_max_rows = self.settings.sort_max_rows;
        let wake = self.waker();
        for i in 0..self.tabs.len() {
            let tab = &mut self.tabs[i];
            let (tab_id, title) = (tab.id, tab.title.clone());
            let path = tab.path.clone();
            if let Some(mv) = tab.merged_mut() {
                if mv.pump(now) {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                if mv.find.restart_at.is_some() {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                if mv.toast.is_some() {
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
                continue;
            }
            let Some(view) = tab.view_mut() else { continue };
            view.set_time_zone(&tz);
            view.sort.max_rows = sort_max_rows;
            let out = view.pump(now);
            if view.st.dirty {
                view.st.dirty = false;
                if let Some(p) = &path {
                    self.gui_state.set_structure(p, view.st.saved());
                }
            }
            if out.repaint {
                ctx.request_repaint();
            }
            if view.poll_tools() {
                ctx.request_repaint();
            }
            if view.tools_busy() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            if let Some(text) = out.copied {
                if out.copy_truncated {
                    view.toast("Selection too large: copied the first part");
                }
                copied = Some(text);
            }
            if let Some(remaining) = view.find.debounce_remaining(now) {
                ctx.request_repaint_after(remaining);
            }
            if let Some(t) = view.filter.hold_until
                && t > now
            {
                ctx.request_repaint_after(t - now);
            }
            if view.toast.is_some() {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
            // Searches and filters run on worker threads without a waker:
            // poll them while they work.
            if view.find.is_running() && !view.find.status.done {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            if view.find.searching {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
            if view.filter.has_job() && !view.filter.status.done {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            let sniffed = view.take_sniffed();
            let job = view
                .take_new_lines()
                .and_then(|(generation, first, count)| {
                    let hl = view.hl.borrow();
                    (hl.has_alert_rules() || hl.has_bookmark_rules()).then(|| AlertJob {
                        tab_id,
                        tab_name: title,
                        doc: Arc::clone(&view.doc),
                        generation,
                        first,
                        count,
                        rules: Arc::clone(&hl.rules),
                        compiled: hl.compiled(),
                        notify,
                    })
                });
            if let Some(lines) = sniffed {
                let sctx = StructureCtx {
                    saved: path
                        .as_deref()
                        .and_then(|p| self.gui_state.structure(p))
                        .cloned(),
                    tz: tz.clone(),
                    wake: Arc::clone(&wake),
                };
                apply_sniffed_profile(&mut self.tabs[i], &profiles, &lines, sctx);
            }
            if let Some(job) = job
                && let Some(w) = &self.alerts
            {
                w.submit(job);
            }
        }
        if let Some(text) = copied {
            ctx.copy_text(text);
        }
    }

    fn drain_alerts(&mut self) {
        let Some(worker) = &self.alerts else { return };
        let events: Vec<AlertEvent> = worker.events.try_iter().collect();
        for ev in events {
            match ev {
                AlertEvent::Hits { tab_id, count } => {
                    let active_id = self.tabs.get(self.active).map(|t| t.id);
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
                        && Some(tab_id) != active_id
                    {
                        tab.badge = tab.badge.saturating_add(count);
                    }
                }
                AlertEvent::Bookmarks {
                    tab_id,
                    generation,
                    lines,
                } => {
                    if let Some(view) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id == tab_id)
                        .and_then(Tab::view_mut)
                        && view.snapshot.generation == generation
                    {
                        view.add_bookmarks(lines);
                    }
                }
            }
        }
    }

    fn track_window(&mut self, ctx: &Context) {
        let geo = ctx.input(|i| {
            let vp = i.viewport();
            let outer = vp.outer_rect?;
            let inner = vp.inner_rect?;
            Some(WindowGeometry {
                x: outer.min.x,
                y: outer.min.y,
                width: inner.width(),
                height: inner.height(),
                maximized: vp.maximized.unwrap_or(false),
            })
        });
        if let Some(g) = geo
            && [g.x, g.y, g.width, g.height].iter().all(|v| v.is_finite())
            && g.width > 0.0
            && g.height > 0.0
        {
            self.window = Some(g);
        }
    }

    // ---------------------------------------------------------------- theme

    /// Applies the theme chosen in the settings to egui.
    pub fn apply_theme(&mut self, ctx: &Context) {
        let system = ctx.system_theme();
        self.last_system_dark = system.map(|t| t == egui::Theme::Dark);
        let theme = select_theme(&self.settings, &self.themes, self.last_system_dark);
        self.colors = Colors::from_theme(&theme);
        let visuals = self.colors.visuals();
        ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
        ctx.set_visuals_of(egui::Theme::Light, visuals);
        self.bump_style();
    }

    /// Chooses a theme (and schedules saving the settings).
    pub fn set_theme(&mut self, choice: oxtail_config::ThemeChoice) {
        self.settings.theme = choice;
        if let Some(ctx) = self.ctx.clone() {
            self.apply_theme(&ctx);
        }
        self.mark_settings_dirty();
    }

    fn follow_system_theme(&mut self, ctx: &Context) {
        if self.settings.theme != oxtail_config::ThemeChoice::System {
            return;
        }
        let now = ctx.system_theme().map(|t| t == egui::Theme::Dark);
        if now != self.last_system_dark {
            self.apply_theme(ctx);
        }
    }

    pub(crate) fn bump_style(&mut self) {
        self.style_epoch = self.style_epoch.wrapping_add(1);
    }

    /// Sets the font size (clamped) and schedules saving the settings.
    pub fn set_font_size(&mut self, size: f32) {
        let size = if size.is_finite() {
            size.clamp(MIN_FONT, MAX_FONT)
        } else {
            self.settings.font_size
        };
        if (size - self.settings.font_size).abs() > f32::EPSILON {
            self.settings.font_size = size;
            self.bump_style();
            self.mark_settings_dirty();
        }
    }

    /// Schedules writing the settings.
    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty_at.get_or_insert_with(Instant::now);
    }

    fn handle_zoom(&mut self, ctx: &Context) {
        let z = ctx.input(|i| i.zoom_delta());
        if (z - 1.0).abs() > 0.001 && !self.text_input_active(ctx) {
            let size = self.settings.font_size * z;
            self.set_font_size(size);
        }
    }

    fn text_input_active(&self, ctx: &Context) -> bool {
        ctx.egui_wants_keyboard_input()
    }

    /// Sets the time zone used to read and show timestamps and schedules
    /// saving the settings.
    pub fn set_timezone(&mut self, tz: oxtail_config::TimezoneSetting) {
        if self.settings.timezone != tz {
            self.settings.timezone = tz;
            self.mark_settings_dirty();
        }
    }

    /// Sets the time gap separator threshold in seconds (`0` switches it off)
    /// and schedules saving the settings.
    pub fn set_gap_threshold(&mut self, secs: f64) {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        if (self.settings.time_gap_threshold_secs - secs).abs() > f64::EPSILON {
            self.settings.time_gap_threshold_secs = secs;
            self.mark_settings_dirty();
        }
    }

    /// Sets wrapping for the active tab and remembers it as the default.
    pub fn set_wrap(&mut self, wrap: bool) {
        self.set_wrap_in_tab(self.active, wrap);
    }

    /// Sets wrapping for tab `index` (the status bar chip of any pane) and
    /// remembers it as the default; the one place wrapping changes.
    pub fn set_wrap_in_tab(&mut self, index: usize, wrap: bool) {
        if let Some(v) = self.tabs.get_mut(index).and_then(Tab::view_mut) {
            v.wrap = wrap;
            v.galleys.borrow_mut().clear();
        }
        self.settings.wrap = wrap;
        self.mark_settings_dirty();
    }

    /// Whether the go-to-time dialog is open.
    pub fn windows_open_goto_time(&self) -> bool {
        self.windows.goto_time.is_some()
    }

    /// The go-to-time dialog's error message, if it shows one.
    pub fn goto_time_error(&self) -> Option<&str> {
        self.windows
            .goto_time
            .as_ref()
            .and_then(|d| d.error.as_deref())
    }

    /// Starts "go to time" for the active tab with what the user typed. The
    /// search runs on a worker; the result is applied by
    /// [`OxTailApp::poll_goto_time`]. Returns an error message for input that
    /// cannot be understood.
    pub fn apply_goto_time(&mut self, input: &str) -> Result<(), String> {
        let tz = crate::colspec::zone_of(&self.settings.timezone);
        let target = parse_goto_time(input, &tz, oxtail_time::jiff::Timestamp::now())?;
        let wake = self.waker();
        let Some(tab) = self.tabs.get(self.active) else {
            return Ok(());
        };
        let tab_id = tab.id;
        let Some(view) = tab.view() else {
            return Ok(());
        };
        let job = GotoJob::start(
            Arc::clone(&view.doc),
            Arc::clone(&view.st.time),
            target,
            wake,
        );
        if let Some(d) = self.windows.goto_time.as_mut() {
            d.job = Some((tab_id, job));
            d.error = None;
        }
        Ok(())
    }

    /// Applies the answer of a running go-to-time search. Never blocks.
    pub(crate) fn poll_goto_time(&mut self, ctx: &Context) {
        let Some(dlg) = self.windows.goto_time.as_mut() else {
            return;
        };
        let Some((tab_id, job)) = dlg.job.as_ref() else {
            return;
        };
        let tab_id = *tab_id;
        let Some(msg) = job.try_recv() else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        dlg.job = None;
        let mut close = false;
        match msg {
            GotoMsg::Found { line, ts } => {
                let tz = crate::colspec::zone_of(&self.settings.timezone);
                if let Some(view) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == tab_id)
                    .and_then(Tab::view_mut)
                {
                    view.jump_to_line(line, true);
                    let at = ts.map(|t| crate::structure::format_timestamp(t, &tz));
                    view.toast(match at {
                        Some(a) => format!("Line {} at {a}", line + 1),
                        None => format!("Line {}", line + 1),
                    });
                }
                close = true;
            }
            GotoMsg::PastEnd { last_line } => {
                if let Some(view) = self
                    .tabs
                    .iter_mut()
                    .find(|t| t.id == tab_id)
                    .and_then(Tab::view_mut)
                {
                    view.jump_to_line(last_line, true);
                    view.toast("That time is after the last line; showing the end");
                }
                close = true;
            }
            GotoMsg::NoTimestamps => {
                dlg.error = Some("No timestamps found in this file".into());
            }
            GotoMsg::Changed => {
                dlg.error = Some("The file changed during the search; try again".into());
            }
            GotoMsg::Cancelled => {}
        }
        if close {
            self.windows.goto_time = None;
        }
        ctx.request_repaint();
    }

    /// Applies the "Go to line" input to the active tab. Returns an error
    /// message for input that cannot be understood.
    pub fn apply_goto(&mut self, input: &str) -> Result<(), String> {
        let goto = crate::goto::parse(input)
            .ok_or_else(|| "Enter a line number, +N, -N or N%".to_string())?;
        let Some(view) = self.active_view_mut() else {
            return Ok(());
        };
        let snap = view.snapshot.clone();
        let current = view
            .cache
            .get(view.cursor_offset())
            .filter(|l| l.number_exact)
            .map_or(0, |l| l.number);
        if let crate::goto::Goto::Percent(p) = goto
            && !snap.lines.exact
        {
            view.jump_fraction((p / 100.0).clamp(0.0, 1.0));
            return Ok(());
        }
        let total = if snap.lines.exact {
            snap.lines.known
        } else {
            snap.lines.estimated_total
        };
        let line = crate::goto::resolve(goto, current, total);
        view.jump_to_line(line, true);
        Ok(())
    }

    // ------------------------------------------------------------- profiles

    /// Uses `profile` (or the default rules) in the active tab.
    pub fn set_profile(&mut self, profile: Option<String>) {
        let profiles = Arc::clone(&self.profiles);
        let tz = self.settings.timezone.clone();
        let wake = self.waker();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.forced_profile.clone_from(&profile);
            tab.customized = false;
            if let Some(v) = tab.view_mut() {
                let p = profile.as_deref().and_then(|n| profiles.by_name(n));
                v.hl.borrow_mut().set_profile(p);
                // The new profile may define columns; a parser the user chose
                // for this file stays.
                let chosen = matches!(
                    v.st.origin,
                    Some(crate::structure::Origin::Chosen | crate::structure::Origin::Restored)
                );
                let saved = chosen.then(|| v.st.saved());
                let profile_for_columns = p.or_else(|| profiles.by_name("Generic"));
                v.begin_structure(
                    crate::tab::structure_input(profile_for_columns, saved, tz),
                    wake,
                );
            }
        }
    }

    /// Opens the rule editor on the active tab's rules.
    pub fn open_rule_editor(&mut self) {
        if let Some(v) = self.active_view() {
            let hl = v.hl.borrow();
            let rules = hl.rules.as_ref().clone();
            let profile = hl.profile.clone();
            drop(hl);
            self.rule_editor.open_with(&rules, profile.as_deref());
        }
    }

    /// Carries out what the rule editor asked for.
    pub fn handle_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::None => {}
            EditorAction::Apply(rules) => {
                if let Some(v) = self.active_view_mut() {
                    let name = v.hl.borrow().profile.clone();
                    v.hl.borrow_mut().set_rules(rules, name);
                    v.galleys.borrow_mut().clear();
                }
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.customized = true;
                }
            }
            EditorAction::Save { name, rules, base } => {
                let base_profile: Option<Profile> = base
                    .as_deref()
                    .and_then(|b| self.profiles.by_name(b))
                    .cloned();
                let mut profile = profile_with_rules(&name, base_profile.as_ref(), &rules);
                // A saved copy of a built-in must not steal its file matches.
                if base_profile.as_ref().is_some_and(|b| b.name != name) {
                    profile.match_files.clear();
                    profile.match_content = None;
                }
                self.send_persist(Job::Profile(profile));
                // Don't wait for the config watcher: it may not be running yet
                // (it starts in the background) or may miss the change. Jobs
                // run in order, so this reload sees the file just written.
                self.send_persist(Job::ReloadProfiles);
                if let Some(v) = self.active_view_mut() {
                    v.hl.borrow_mut().set_rules(rules, Some(name.clone()));
                    v.galleys.borrow_mut().clear();
                }
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.forced_profile = Some(name);
                    t.customized = false;
                }
            }
        }
    }

    // ------------------------------------------------------------ persisting

    pub(crate) fn send_persist(&self, job: Job) {
        if let Some(p) = &self.persist {
            p.send(job);
        }
    }

    /// The session as it would be saved now.
    pub fn current_session(&self) -> Session {
        let mut s = build_session(
            &self.tabs,
            self.active,
            &self.session,
            self.window,
            self.settings.recent_files_limit.max(1),
        );
        s.layout = self.session_layout();
        s
    }

    fn periodic(&mut self, ctx: &Context, now: Instant) {
        if let Some(t) = self.settings_dirty_at {
            if now.duration_since(t) >= SETTINGS_SAVE_DELAY {
                self.settings_dirty_at = None;
                self.send_persist(Job::Settings(self.settings.clone()));
            } else {
                ctx.request_repaint_after(SETTINGS_SAVE_DELAY);
            }
        }
        if self.history_dirty {
            self.history_dirty = false;
            self.send_persist(Job::History(self.history.clone()));
        }
        if now.duration_since(self.last_session_save) >= SESSION_SAVE_INTERVAL {
            self.last_session_save = now;
            self.save_session_if_changed();
        }
        ctx.request_repaint_after(SESSION_SAVE_INTERVAL);
    }

    fn save_session_if_changed(&mut self) {
        let s = self.current_session();
        if self.last_saved_session.as_ref() != Some(&s) {
            self.session = s.clone();
            self.last_saved_session = Some(s.clone());
            self.send_persist(Job::Session(s));
        }
        self.save_gui_state_if_changed();
    }

    fn save_gui_state_if_changed(&mut self) {
        self.gui_state.panes = self.pane_state();
        self.gui_state.merged = self.merged_defs();
        if self.last_saved_gui_state.as_ref() != Some(&self.gui_state) {
            self.last_saved_gui_state = Some(self.gui_state.clone());
            self.send_persist(Job::GuiState(Box::new(self.gui_state.clone())));
        }
    }

    /// Saves everything and stops the worker threads (called when the window
    /// closes).
    pub fn shutdown(&mut self) {
        let s = self.current_session();
        self.send_persist(Job::Session(s));
        self.save_gui_state_if_changed();
        if self.settings_dirty_at.take().is_some() {
            self.send_persist(Job::Settings(self.settings.clone()));
        }
        if self.history_dirty {
            self.history_dirty = false;
            self.send_persist(Job::History(self.history.clone()));
        }
        if let Some(mut p) = self.persist.take() {
            // The only place the GUI waits for a worker: the last session
            // must reach the disk before the process exits.
            p.shutdown(Duration::from_secs(2));
        }
    }
}

impl eframe::App for OxTailApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }

    fn auto_save_interval(&self) -> Duration {
        SESSION_SAVE_INTERVAL
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.colors.background.to_normalized_gamma_f32()
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// Native window options for `renderer`, restoring `window` if known.
pub fn native_options(
    renderer: eframe::Renderer,
    window: Option<WindowGeometry>,
) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("OxTail")
        .with_app_id("oxtail")
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([480.0, 240.0])
        .with_icon(crate::icon::icon_data())
        .with_drag_and_drop(true);
    if let Some(w) = window {
        viewport = viewport
            .with_inner_size([w.width.max(480.0), w.height.max(240.0)])
            .with_position([w.x, w.y])
            .with_maximized(w.maximized);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    };
    // eframe queues two frames by default (high throughput); one keeps input
    // and drawing a frame closer together, which is what a viewer needs.
    options.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY;
    options
}

/// Most files opened from one dropped folder.
pub const MAX_FILES_PER_FOLDER: usize = 50;

/// Turns dropped paths into files to open: files stay, folders are replaced by
/// the regular files directly inside them (sorted by name, at most
/// [`MAX_FILES_PER_FOLDER`] per folder). Reads the file system: call it from a
/// worker thread.
pub fn expand_dropped(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => {
                let mut files: Vec<PathBuf> = std::fs::read_dir(p)
                    .map(|rd| {
                        rd.flatten()
                            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                            .map(|e| e.path())
                            .collect()
                    })
                    .unwrap_or_default();
                files.sort();
                files.truncate(MAX_FILES_PER_FOLDER);
                out.extend(files);
            }
            _ => out.push(p.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_expanded_to_their_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.log"), "x").unwrap();
        std::fs::write(dir.path().join("a.log"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let single = dir.path().join("b.log");
        let got = expand_dropped(&[dir.path().to_path_buf(), single.clone()]);
        assert_eq!(
            got,
            vec![dir.path().join("a.log"), dir.path().join("b.log"), single]
        );
        // Missing paths are passed through so the tab can report the error.
        let missing = dir.path().join("nope.log");
        assert_eq!(
            expand_dropped(std::slice::from_ref(&missing)),
            vec![missing]
        );
    }

    #[test]
    fn a_big_folder_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_FILES_PER_FOLDER + 10) {
            std::fs::write(dir.path().join(format!("f{i:03}.log")), "x").unwrap();
        }
        assert_eq!(
            expand_dropped(&[dir.path().to_path_buf()]).len(),
            MAX_FILES_PER_FOLDER
        );
    }
}
