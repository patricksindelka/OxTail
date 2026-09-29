//! The [`Document`]: one actor thread per open file.
//!
//! # Model
//!
//! A `Document` owns, on a dedicated thread, everything needed to show a file:
//! the raw source, the optional UTF-8 spool, the block cache, the sparse line
//! index, the change watcher and the follow state. The UI (or any other
//! caller) only ever
//!
//! * reads the latest [`DocSnapshot`] (`Arc`, never blocks),
//! * sends requests ([`Document::request_lines`], never blocks), and
//! * drains [`DocEvent`]s from [`Document::events`] with `try_recv`.
//!
//! [`Document::set_waker`] registers a callback that fires whenever an event
//! is queued, so a GUI can call `request_repaint()`.
//!
//! Worker threads (search, filter builders, ...) may use the *blocking*
//! helpers: [`Document::source`], [`Document::read_lines_blocking`],
//! [`Document::line_of_offset`] and [`Document::offset_of_line`]. These read
//! shared state and may do a block read; **never call them from the UI thread**.
//!
//! # Pipeline
//!
//! ```text
//! raw file --(detect encoding)--> [spool: UTF-8 copy, only if needed] --> view
//! view --(chunked memchr scan)--> sparse LineIndex (Arc<RwLock>, brief locks)
//! ```
//!
//! Plain UTF-8/ASCII files use the file itself as the view. Everything a
//! caller sees (offsets, [`Document::source`]) refers to the view.
//!
//! # Generations
//!
//! `generation` starts at 0 and is bumped by truncation, rotation and encoding
//! changes. Line results carry the generation they were read in; drop results
//! whose generation is older than the current snapshot's.

use std::any::Any;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use parking_lot::RwLock;

use crate::cache::{BlockCache, CachedSource, DEFAULT_CACHE_BYTES};
use crate::encoding::{Detected, EncodingChoice, LineEnding, SAMPLE_LEN, TextEncoding, detect};
use crate::error::CoreError;
use crate::follow::{AdaptiveInterval, Change, FollowMode, FsWatcher, classify};
use crate::index::{
    DEFAULT_SPACING, LineIndex, SCAN_CHUNK, Scanner, find_tail_start, line_start_before,
};
use crate::line::{DEFAULT_MAX_DISPLAY_LEN, Line, read_lines};
use crate::source::{FileSource, PathState, ReadAt, SwitchSource};
use crate::spool::{Spool, create_temp};

/// Bytes of the view indexed per actor step (keeps commands responsive).
const INDEX_STEP: u64 = 8 * 1024 * 1024;
/// Raw bytes transcoded per actor step.
const SPOOL_STEP: usize = 4 * 1024 * 1024;
/// Backlog above which the state reads `Indexing` instead of `Ready`.
const INDEXING_BACKLOG: u64 = 4 * 1024 * 1024;
/// Longest backward scan for a tail request (guards against endless lines).
const MAX_BACKSCAN: u64 = 32 * 1024 * 1024;
/// Longest backward scan to find the line start for a byte position.
const MAX_ALIGN_BACK: u64 = 4 * 1024 * 1024;
/// Upper bound on lines returned by one request.
const MAX_REQUEST_LINES: usize = 20_000;
/// A file counts as "being written" for this long after its last growth.
const WRITING_WINDOW: Duration = Duration::from_secs(3);
/// Minimum spacing between actor polls triggered by watcher pokes.
const MIN_POLL_GAP: Duration = Duration::from_millis(20);
/// Minimum spacing between `IndexProgress` events / snapshot publishes.
const PROGRESS_GAP: Duration = Duration::from_millis(50);

/// Options for [`Document::open`] and friends.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Encoding: detect automatically (default) or force one.
    pub encoding: EncodingChoice,
    /// Follow by name (default, `tail -F`) or by handle (`tail -f`).
    pub follow_mode: FollowMode,
    /// Watch for growth, truncation and rotation (default `true`). When
    /// `false` the file is examined once and treated as static.
    pub follow: bool,
    /// Directory for spool files; `None` or an unusable directory falls back
    /// to `std::env::temp_dir()`.
    pub spool_dir: Option<PathBuf>,
    /// Block cache capacity in bytes (default 64 MiB).
    pub cache_bytes: usize,
    /// Longest line text kept for display, in bytes (default 16 KiB).
    pub max_display_len: usize,
    /// If set, the actor sends an initial [`DocEvent::Lines`] with
    /// [`RequestId::INITIAL`] holding the last `n` lines as soon as possible.
    pub start_at_tail: Option<usize>,
    /// Bytes between index checkpoints (default 64 KiB; tests use small values).
    pub index_spacing: u64,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            encoding: EncodingChoice::Auto,
            follow_mode: FollowMode::Name,
            follow: true,
            spool_dir: None,
            cache_bytes: DEFAULT_CACHE_BYTES,
            max_display_len: DEFAULT_MAX_DISPLAY_LEN,
            start_at_tail: None,
            index_spacing: DEFAULT_SPACING,
        }
    }
}

/// Identifies a [`LineRequest`]; echoed in the matching [`DocEvent::Lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl RequestId {
    /// The id used for the automatic `start_at_tail` result.
    pub const INITIAL: RequestId = RequestId(0);
}

/// What lines to fetch. See [`Document::request_lines`].
#[derive(Debug, Clone, PartialEq)]
pub enum LineRequest {
    /// `count` lines starting at 0-based line `first`. Beyond the indexed
    /// region the start is estimated and lines are flagged `number_exact: false`.
    Range {
        /// First line (0-based).
        first: u64,
        /// Maximum number of lines.
        count: usize,
    },
    /// The last `count` lines (found by scanning backward from the end, so it
    /// works before indexing finishes).
    Tail {
        /// Maximum number of lines.
        count: usize,
    },
    /// One line for each given line-start byte offset (used by search and
    /// filter views). Offsets at or past the end yield no line, so the result
    /// may be shorter than the request; match results to requests by
    /// `Line::offset`.
    AtOffsets(Vec<u64>),
    /// `count` lines starting at the line containing byte
    /// `fraction * utf8_len`: the scrollbar mapping used before indexing
    /// completes.
    ByteFraction {
        /// Position in `0.0..=1.0`.
        fraction: f64,
        /// Maximum number of lines.
        count: usize,
    },
}

/// Progress of the document.
#[derive(Debug, Clone, PartialEq)]
pub enum DocState {
    /// The actor is starting and has not looked at the file yet.
    Opening,
    /// A large backlog is being indexed; `fraction` is in `0.0..=1.0`.
    Indexing {
        /// Indexed share of the view.
        fraction: f64,
    },
    /// Everything known is indexed (following continues).
    Ready,
    /// Something failed; the message is user-presentable. Cleared when the
    /// next operation succeeds.
    Error(String),
}

/// Kind of a [`Notice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// The file shrank; the view was reset.
    Truncated,
    /// The path now names another file; it was reopened and the view reset.
    Rotated,
    /// The path no longer exists (the old handle is still followed).
    Removed,
    /// The encoding was changed manually; the view was rebuilt.
    EncodingChanged,
}

/// The last notable event, kept in the snapshot for banners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    /// What happened.
    pub kind: NoticeKind,
    /// When (wall clock).
    pub at: SystemTime,
}

/// Line counts of a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCount {
    /// Lines known from the indexed region.
    pub known: u64,
    /// `true` when the whole view is indexed, i.e. `known` is the total.
    pub exact: bool,
    /// Best estimate of the total (equals `known` when `exact`).
    pub estimated_total: u64,
}

/// Immutable, cheaply shared view of a document's status. See [`Document::snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct DocSnapshot {
    /// Name to show in tabs (file name, or the name given to `from_source`).
    pub display_name: String,
    /// Full path if the document is backed by a file.
    pub path: Option<PathBuf>,
    /// Encoding of the raw data (detected or forced).
    pub encoding: TextEncoding,
    /// Line terminator style of the raw data.
    pub line_ending: LineEnding,
    /// `true` when the view is a spooled transcoding rather than the file.
    pub spooled: bool,
    /// Length of the UTF-8 view in bytes.
    pub utf8_len: u64,
    /// Bytes of the view covered by the line index.
    pub indexed_bytes: u64,
    /// Line counts.
    pub lines: LineCount,
    /// Progress/error state.
    pub state: DocState,
    /// Bumped on truncation, rotation and encoding change.
    pub generation: u64,
    /// The last truncation/rotation/removal, if any.
    pub last_event: Option<Notice>,
    /// `true` if the file grew in the last few seconds (or a stdin feed is
    /// still open).
    pub writing: bool,
    /// `true` while a followed path does not exist.
    pub file_missing: bool,
}

/// Position of a byte offset in line terms. See [`Document::line_of_offset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePosition {
    /// 0-based line number containing the offset.
    pub line: u64,
    /// `false` if the offset lies beyond the indexed region (estimate).
    pub exact: bool,
    /// Start offset of that line (best effort when not exact).
    pub start: u64,
}

/// Notifications and results from the actor, see [`Document::events`].
#[derive(Debug, Clone)]
pub enum DocEvent {
    /// Answer to a [`Document::request_lines`] call. Empty on error or when
    /// the request matched nothing. Ignore if `generation` is stale.
    Lines {
        /// The request this answers.
        id: RequestId,
        /// Generation the lines were read in.
        generation: u64,
        /// The lines.
        lines: Vec<Line>,
    },
    /// The UTF-8 view grew (append, or more of a spool became available).
    Grew {
        /// New length of the view.
        utf8_len: u64,
    },
    /// Indexing advanced (throttled; the last event has `indexed == total`).
    IndexProgress {
        /// Bytes indexed.
        indexed_bytes: u64,
        /// Bytes to index.
        total_bytes: u64,
    },
    /// The file shrank. The view was reset; `generation` is the new one.
    Truncated {
        /// New generation.
        generation: u64,
        /// When it was noticed.
        at: SystemTime,
    },
    /// The path now names a different file. The view was reset.
    Rotated {
        /// New generation.
        generation: u64,
        /// When it was noticed.
        at: SystemTime,
    },
    /// The followed path disappeared (the open handle is still followed).
    Removed {
        /// When it was noticed.
        at: SystemTime,
    },
    /// The encoding override changed and the view was rebuilt.
    EncodingChanged {
        /// New generation.
        generation: u64,
    },
    /// A non-fatal error (the message is user-presentable).
    Error(String),
}

/// Callback fired (from the actor thread) whenever an event is queued.
type Waker = Arc<dyn Fn() + Send + Sync>;

enum Cmd {
    Lines(RequestId, u64, LineRequest),
    Poke,
    SetEncoding(EncodingChoice),
    Shutdown,
}

/// State shared between the actor and the [`Document`] handle.
struct Shared {
    name: String,
    path: Option<PathBuf>,
    view: Arc<SwitchSource>,
    cached: CachedSource,
    cache: Arc<BlockCache>,
    index: RwLock<LineIndex>,
    snapshot: RwLock<Arc<DocSnapshot>>,
    generation: Arc<AtomicU64>,
    view_len: AtomicU64,
    max_display_len: usize,
    events_tx: Sender<DocEvent>,
    waker: RwLock<Option<Waker>>,
    shutdown: Arc<AtomicBool>,
}

impl Shared {
    fn emit(&self, ev: DocEvent) {
        let _ = self.events_tx.send(ev);
        let waker = self.waker.read().clone();
        if let Some(w) = waker {
            w();
        }
    }

    fn limit(&self) -> u64 {
        self.view_len.load(Ordering::Acquire)
    }

    /// Line number of the line starting at `off`: exact inside the indexed
    /// region, estimated beyond it.
    fn number_at(&self, idx: &LineIndex, off: u64) -> io::Result<(u64, bool)> {
        if off <= idx.indexed_bytes() {
            Ok((idx.line_of_offset(&self.cached, off)?.line, true))
        } else {
            Ok((idx.estimate_line_of_offset(off), false))
        }
    }

    fn read_request(&self, req: &LineRequest) -> io::Result<Vec<Line>> {
        let limit = self.limit();
        let max = self.max_display_len;
        match req {
            LineRequest::Range { first, count } => {
                let count = (*count).min(MAX_REQUEST_LINES);
                let (start, number, exact) = {
                    let idx = self.index.read();
                    if *first < idx.total_lines() {
                        match idx.offset_of_line(&self.cached, *first)? {
                            Some(o) => (o, *first, true),
                            None => return Ok(Vec::new()),
                        }
                    } else if idx.indexed_bytes() >= limit || limit == 0 {
                        return Ok(Vec::new());
                    } else {
                        let e = idx
                            .estimate_offset_of_line(*first)
                            .min(limit.saturating_sub(1));
                        let s = self.line_start_containing(e)?;
                        (s, idx.estimate_line_of_offset(s), false)
                    }
                };
                read_lines(&self.cached, start, count, limit, number, exact, max)
            }
            LineRequest::Tail { count } => {
                let count = (*count).clamp(1, MAX_REQUEST_LINES);
                if limit == 0 {
                    return Ok(Vec::new());
                }
                let (start, _) = find_tail_start(&self.cached, limit, count, MAX_BACKSCAN)?;
                let (number, exact) = {
                    let idx = self.index.read();
                    self.number_at(&idx, start)?
                };
                read_lines(&self.cached, start, count, limit, number, exact, max)
            }
            LineRequest::AtOffsets(offsets) => {
                let mut out = Vec::with_capacity(offsets.len().min(MAX_REQUEST_LINES));
                for &off in offsets.iter().take(MAX_REQUEST_LINES) {
                    if off >= limit {
                        continue;
                    }
                    let (number, exact) = {
                        let idx = self.index.read();
                        self.number_at(&idx, off)?
                    };
                    out.extend(read_lines(&self.cached, off, 1, limit, number, exact, max)?);
                }
                Ok(out)
            }
            LineRequest::ByteFraction { fraction, count } => {
                let count = (*count).min(MAX_REQUEST_LINES);
                if limit == 0 {
                    return Ok(Vec::new());
                }
                let f = if fraction.is_finite() {
                    fraction.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let off = ((f * limit as f64) as u64).min(limit - 1);
                let start = self.line_start_containing(off)?;
                let (number, exact) = {
                    let idx = self.index.read();
                    self.number_at(&idx, start)?
                };
                read_lines(&self.cached, start, count, limit, number, exact, max)
            }
        }
    }

    /// Start of the line containing byte `off` (bounded backward scan).
    fn line_start_containing(&self, off: u64) -> io::Result<u64> {
        if off == 0 {
            return Ok(0);
        }
        let mut b = [0u8; 1];
        if self.cached.read_at(off - 1, &mut b)? == 1 && b[0] == b'\n' {
            return Ok(off);
        }
        line_start_before(&self.cached, off, MAX_ALIGN_BACK)
    }
}

/// An open file (or in-memory/stdin source) with its actor thread.
///
/// Dropping the `Document` stops the actor and the watcher, waits for the
/// thread (at most one step of work, a few milliseconds) and deletes any
/// spool files.
pub struct Document {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    events_rx: Receiver<DocEvent>,
    next_id: AtomicU64,
    join: Option<JoinHandle<()>>,
    stop_reader: Arc<AtomicBool>,
}

/// Everything the actor thread needs to start.
struct Setup {
    raw: Arc<dyn ReadAt>,
    file: Option<Arc<FileSource>>,
    opts: OpenOptions,
    /// Set while a stdin-like feed is still writing.
    source_open: Option<Arc<AtomicBool>>,
    keep_alive: Vec<Box<dyn Any + Send>>,
}

impl Document {
    /// Opens `path` read-only (shared access, never blocking the writer) and
    /// starts following it.
    ///
    /// Only the `open` syscall happens on the calling thread; call this from a
    /// worker thread if the file may live on a slow network share. Detection,
    /// indexing and everything else run on the actor thread.
    pub fn open(path: impl AsRef<Path>, opts: OpenOptions) -> Result<Document, CoreError> {
        let path = std::path::absolute(path.as_ref()).unwrap_or_else(|_| path.as_ref().into());
        let file = FileSource::open(&path).map_err(|e| CoreError::io(&path, e))?;
        let file = Arc::new(file);
        if std::fs::metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err(CoreError::io(
                &path,
                io::Error::new(io::ErrorKind::InvalidInput, "is a directory"),
            ));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::spawn(
            name,
            Some(path),
            Setup {
                raw: file.clone(),
                file: Some(file),
                opts,
                source_open: None,
                keep_alive: Vec::new(),
            },
        )
    }

    /// Wraps any [`ReadAt`] (a `MemSource` in tests, say) with default
    /// options. The source is polled for growth and truncation; rotation
    /// does not apply.
    pub fn from_source(src: Arc<dyn ReadAt>, name: impl Into<String>) -> Document {
        Self::from_source_with(src, name, OpenOptions::default())
    }

    /// Like [`Document::from_source`] with explicit options.
    pub fn from_source_with(
        src: Arc<dyn ReadAt>,
        name: impl Into<String>,
        opts: OpenOptions,
    ) -> Document {
        Self::spawn(
            name.into(),
            None,
            Setup {
                raw: src,
                file: None,
                opts,
                source_open: None,
                keep_alive: Vec::new(),
            },
        )
        .expect(
            "spawning a document thread for an in-memory source only fails on OS thread exhaustion",
        )
    }

    /// Follows standard input: a thread copies stdin into a spool file in
    /// `spool_dir` (or the temp dir) and the document follows that file.
    pub fn from_stdin(spool_dir: Option<&Path>) -> Result<Document, CoreError> {
        Self::from_reader(Box::new(io::stdin()), "<stdin>", spool_dir)
    }

    /// Like [`Document::from_stdin`] for any reader (used by tests). The
    /// copying thread ends at EOF, on a read error, or when the document is
    /// dropped (it may linger until its blocked `read` returns).
    pub fn from_reader(
        mut reader: Box<dyn Read + Send>,
        name: impl Into<String>,
        spool_dir: Option<&Path>,
    ) -> Result<Document, CoreError> {
        let tmp = create_temp(spool_dir, "oxtail-stdin-")?;
        let tmp_path = tmp.path().to_path_buf();
        let mut writer = tmp
            .as_file()
            .try_clone()
            .map_err(|e| CoreError::io(&tmp_path, e))?;
        let read_file = tmp.reopen().map_err(|e| CoreError::io(&tmp_path, e))?;
        let open_flag = Arc::new(AtomicBool::new(true));
        let raw: Arc<dyn ReadAt> = Arc::new(FileSource::from_file(read_file));
        let doc = Self::spawn(
            name.into(),
            None,
            Setup {
                raw,
                file: None,
                opts: OpenOptions::default(),
                source_open: Some(open_flag.clone()),
                keep_alive: vec![Box::new(tmp)],
            },
        )?;
        let stop = doc.stop_reader.clone();
        let tx = doc.tx.clone();
        std::thread::Builder::new()
            .name("oxtail-stdin".into())
            .spawn(move || {
                use std::io::Write;
                let mut buf = vec![0u8; 64 * 1024];
                while !stop.load(Ordering::Relaxed) {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if writer.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                open_flag.store(false, Ordering::Release);
                let _ = tx.send(Cmd::Poke);
            })
            .map_err(|e| CoreError::Actor(e.to_string()))?;
        Ok(doc)
    }

    fn spawn(name: String, path: Option<PathBuf>, setup: Setup) -> Result<Document, CoreError> {
        let generation = Arc::new(AtomicU64::new(0));
        let cache = Arc::new(BlockCache::new(setup.opts.cache_bytes));
        let view = Arc::new(SwitchSource::new(setup.raw.clone()));
        let cached = CachedSource::new(view.clone(), cache.clone(), generation.clone());
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (tx, rx) = crossbeam_channel::unbounded();
        let shutdown = Arc::new(AtomicBool::new(false));
        let initial = DocSnapshot {
            display_name: name.clone(),
            path: path.clone(),
            encoding: TextEncoding::UTF_8,
            line_ending: LineEnding::Lf,
            spooled: false,
            utf8_len: 0,
            indexed_bytes: 0,
            lines: LineCount {
                known: 0,
                exact: true,
                estimated_total: 0,
            },
            state: DocState::Opening,
            generation: 0,
            last_event: None,
            writing: false,
            file_missing: false,
        };
        let shared = Arc::new(Shared {
            name,
            path,
            view,
            cached,
            cache,
            index: RwLock::new(LineIndex::with_spacing(setup.opts.index_spacing)),
            snapshot: RwLock::new(Arc::new(initial)),
            generation,
            view_len: AtomicU64::new(0),
            max_display_len: setup.opts.max_display_len.max(16),
            events_tx,
            waker: RwLock::new(None),
            shutdown: shutdown.clone(),
        });
        let actor = Actor::new(shared.clone(), rx, tx.clone(), setup);
        let join = std::thread::Builder::new()
            .name("oxtail-doc".into())
            .spawn(move || actor.run())
            .map_err(|e| CoreError::Actor(e.to_string()))?;
        Ok(Document {
            shared,
            tx,
            events_rx,
            next_id: AtomicU64::new(1),
            join: Some(join),
            stop_reader: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The latest status. Never blocks (a brief lock, no I/O).
    pub fn snapshot(&self) -> Arc<DocSnapshot> {
        self.shared.snapshot.read().clone()
    }

    /// The event channel. Drain it with `try_recv` on the UI thread.
    pub fn events(&self) -> &Receiver<DocEvent> {
        &self.events_rx
    }

    /// Registers a callback invoked (on the actor thread) whenever an event
    /// is queued, e.g. `move || ctx.request_repaint()`. Replaces any earlier one.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.shared.waker.write() = Some(Arc::from(waker));
    }

    /// Queues a request; the answer arrives as [`DocEvent::Lines`] with the
    /// returned id. Never blocks.
    ///
    /// The request is bound to the generation current *now*: if the file is
    /// truncated, rotated or re-decoded before the actor gets to it, the
    /// answer is empty and tagged with this (old) generation, so the caller
    /// drops it like any other stale result.
    pub fn request_lines(&self, req: LineRequest) -> RequestId {
        self.request_lines_in(self.generation(), req)
    }

    /// Like [`Document::request_lines`], but bound to `generation` (the one
    /// the request's offsets or line numbers were computed in, e.g. by a
    /// search worker). If the document has moved on by the time the actor
    /// serves it, the answer is empty and tagged with `generation`.
    pub fn request_lines_in(&self, generation: u64, req: LineRequest) -> RequestId {
        let id = RequestId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let _ = self.tx.send(Cmd::Lines(id, generation, req));
        id
    }

    /// Asks the actor to check the file now instead of at the next poll tick.
    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Poke);
    }

    /// Changes the encoding (manual override) and rebuilds the view. Bumps
    /// the generation and emits [`DocEvent::EncodingChanged`].
    pub fn set_encoding(&self, choice: EncodingChoice) {
        let _ = self.tx.send(Cmd::SetEncoding(choice));
    }

    /// The current generation (same as `snapshot().generation`, but fresher).
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// The UTF-8 view as a random-access source, for scanning (search reads
    /// it directly and bypasses the UI block cache). The `Arc` stays valid
    /// across truncation, rotation and encoding changes; check `len()`.
    pub fn source(&self) -> Arc<dyn ReadAt> {
        self.shared.view.clone()
    }

    /// Line containing byte `offset` of the view. Exact inside the indexed
    /// region, estimated beyond it. Reads shared state and may read one
    /// block: **for worker threads, not the UI thread**.
    pub fn line_of_offset(&self, offset: u64) -> io::Result<LinePosition> {
        let idx = self.shared.index.read();
        if offset <= idx.indexed_bytes() {
            let p = idx.line_of_offset(&self.shared.cached, offset)?;
            Ok(LinePosition {
                line: p.line,
                exact: true,
                start: p.start,
            })
        } else {
            let line = idx.estimate_line_of_offset(offset);
            drop(idx);
            let start = self.shared.line_start_containing(offset)?;
            Ok(LinePosition {
                line,
                exact: false,
                start,
            })
        }
    }

    /// Like [`Document::line_of_offset`], but also returns the generation the
    /// answer belongs to (captured before the read; if the generation changed
    /// meanwhile, an [`io::ErrorKind::Interrupted`] error is returned so the
    /// caller retries with fresh offsets).
    pub fn line_of_offset_with_generation(&self, offset: u64) -> io::Result<(u64, LinePosition)> {
        let generation = self.generation();
        let pos = self.line_of_offset(offset)?;
        if self.generation() != generation {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "document generation changed while reading",
            ));
        }
        Ok((generation, pos))
    }

    /// Start offset of line `line`, or `None` if it is not (yet) in the
    /// indexed region. Same threading caveat as [`Document::line_of_offset`].
    pub fn offset_of_line(&self, line: u64) -> io::Result<Option<u64>> {
        self.shared
            .index
            .read()
            .offset_of_line(&self.shared.cached, line)
    }

    /// Synchronously reads `count` lines from line `first` (like
    /// [`LineRequest::Range`]). Errors yield an empty vector. For worker
    /// threads only.
    pub fn read_lines_blocking(&self, first: u64, count: usize) -> Vec<Line> {
        self.read_lines_blocking_with_generation(first, count).1
    }

    /// Like [`Document::read_lines_blocking`], but also returns the
    /// generation the lines belong to. The generation is captured before the
    /// read; if it changed while reading, the lines are discarded (empty
    /// vector) so they can never be paired with the wrong generation.
    pub fn read_lines_blocking_with_generation(
        &self,
        first: u64,
        count: usize,
    ) -> (u64, Vec<Line>) {
        let generation = self.generation();
        let lines = self
            .shared
            .read_request(&LineRequest::Range { first, count })
            .unwrap_or_default();
        if self.generation() != generation {
            return (generation, Vec::new());
        }
        (generation, lines)
    }

    /// Display name (file name or the name given at creation).
    pub fn display_name(&self) -> &str {
        &self.shared.name
    }

    /// Path of the backing file, if any.
    pub fn path(&self) -> Option<&Path> {
        self.shared.path.as_deref()
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.stop_reader.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The actor: owns the pipeline and runs on its own thread.
struct Actor {
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    tx: Sender<Cmd>,
    raw: Arc<dyn ReadAt>,
    file: Option<Arc<FileSource>>,
    opts: OpenOptions,
    choice: EncodingChoice,
    detected: Option<Detected>,
    spool: Option<Spool>,
    /// Raw length accepted so far (what we know exists).
    raw_target: u64,
    /// Raw bytes already transcoded into the spool.
    raw_pos: u64,
    /// Length of the UTF-8 view.
    view_len: u64,
    /// Mirror of the index's `indexed_bytes` (only this thread writes it).
    indexed: u64,
    buf: Vec<u8>,
    interval: AdaptiveInterval,
    poke_pending: Arc<AtomicBool>,
    poll_deadline: Option<Instant>,
    last_poll: Instant,
    watcher: Option<FsWatcher>,
    missing: bool,
    last_growth: Option<Instant>,
    notice: Option<Notice>,
    error: Option<String>,
    stalled: bool,
    initialised: bool,
    initial_tail: Option<usize>,
    source_open: Option<Arc<AtomicBool>>,
    last_publish: Instant,
    last_progress: Instant,
    dirty: bool,
    _keep_alive: Vec<Box<dyn Any + Send>>,
}

impl Actor {
    fn new(shared: Arc<Shared>, rx: Receiver<Cmd>, tx: Sender<Cmd>, setup: Setup) -> Self {
        let now = Instant::now();
        Actor {
            shared,
            rx,
            tx,
            raw: setup.raw,
            file: setup.file,
            choice: setup.opts.encoding,
            initial_tail: setup.opts.start_at_tail,
            opts: setup.opts,
            detected: None,
            spool: None,
            raw_target: 0,
            raw_pos: 0,
            view_len: 0,
            indexed: 0,
            buf: Vec::new(),
            interval: AdaptiveInterval::default(),
            poke_pending: Arc::new(AtomicBool::new(false)),
            poll_deadline: None,
            last_poll: now,
            watcher: None,
            missing: false,
            last_growth: None,
            notice: None,
            error: None,
            stalled: false,
            initialised: false,
            source_open: setup.source_open,
            last_publish: now,
            last_progress: now,
            dirty: true,
            _keep_alive: setup.keep_alive,
        }
    }

    fn run(mut self) {
        self.start_watcher();
        self.poll();
        self.initialised = true;
        self.publish();
        let mut first_step_done = false;
        loop {
            if self.shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            loop {
                match self.rx.try_recv() {
                    Ok(cmd) => {
                        if !self.handle(cmd) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if self.has_work() {
                self.step();
                if !first_step_done {
                    first_step_done = true;
                    self.send_initial_tail();
                }
                if self.opts.follow && self.last_poll.elapsed() >= self.interval.current() {
                    self.poll();
                }
                self.maybe_publish(false);
                continue;
            }
            if !first_step_done {
                first_step_done = true;
                self.send_initial_tail();
            }
            self.maybe_publish(true);
            let received = match self.next_wait() {
                Some(wait) => self.rx.recv_timeout(wait),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => self.poll(),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn start_watcher(&mut self) {
        if !self.opts.follow {
            return;
        }
        let Some(path) = self.shared.path.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let pending = self.poke_pending.clone();
        self.watcher = FsWatcher::new(&path, move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = tx.send(Cmd::Poke);
            }
        });
    }

    fn next_wait(&self) -> Option<Duration> {
        if !self.opts.follow && self.source_open.is_none() {
            return None;
        }
        let tick = self.interval.current();
        Some(match self.poll_deadline {
            Some(d) => d.saturating_duration_since(Instant::now()).min(tick),
            None => tick,
        })
    }

    /// Returns `false` when the actor should stop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Shutdown => return false,
            Cmd::Poke => {
                self.poke_pending.store(false, Ordering::Release);
                if self.last_poll.elapsed() >= MIN_POLL_GAP {
                    self.poll();
                } else {
                    self.poll_deadline = Some(self.last_poll + MIN_POLL_GAP);
                }
            }
            Cmd::Lines(id, requested_gen, req) => {
                if requested_gen != self.shared.generation.load(Ordering::Acquire) {
                    self.shared.emit(DocEvent::Lines {
                        id,
                        generation: requested_gen,
                        lines: Vec::new(),
                    });
                    return true;
                }
                let lines = match self.shared.read_request(&req) {
                    Ok(l) => l,
                    Err(e) => {
                        self.shared
                            .emit(DocEvent::Error(format!("read failed: {e}")));
                        Vec::new()
                    }
                };
                self.shared.emit(DocEvent::Lines {
                    id,
                    generation: requested_gen,
                    lines,
                });
            }
            Cmd::SetEncoding(choice) => {
                self.choice = choice;
                self.reset_pipeline();
                self.notice = Some(Notice {
                    kind: NoticeKind::EncodingChanged,
                    at: SystemTime::now(),
                });
                self.raw_target = 0;
                self.poll();
                self.publish();
                self.shared.emit(DocEvent::EncodingChanged {
                    generation: self.shared.generation.load(Ordering::Acquire),
                });
            }
        }
        true
    }

    fn send_initial_tail(&mut self) {
        if let Some(n) = self.initial_tail.take() {
            let lines = self
                .shared
                .read_request(&LineRequest::Tail { count: n })
                .unwrap_or_default();
            self.shared.emit(DocEvent::Lines {
                id: RequestId::INITIAL,
                generation: self.shared.generation.load(Ordering::Acquire),
                lines,
            });
        }
    }

    fn has_work(&self) -> bool {
        if self.stalled {
            return false;
        }
        (self.spool.is_some() && self.raw_pos < self.raw_target) || self.indexed < self.view_len
    }

    /// Checks the file for growth, truncation and rotation.
    fn poll(&mut self) {
        self.last_poll = Instant::now();
        self.poll_deadline = None;
        self.poke_pending.store(false, Ordering::Release);
        self.stalled = false;
        let cur_len = match self.raw.len() {
            Ok(l) => l,
            Err(e) => {
                self.fail(format!("cannot stat file: {e}"));
                return;
            }
        };
        let path_state = match &self.file {
            Some(f) => f.path_state(),
            None => PathState::Same,
        };
        let mode = if self.file.is_some() {
            self.opts.follow_mode
        } else {
            FollowMode::Handle
        };
        let change = if self.initialised && !self.opts.follow && self.raw_target != 0 {
            Change::Unchanged
        } else {
            classify(mode, path_state, self.raw_target, cur_len)
        };
        match change {
            Change::Unchanged => self.interval.on_idle(),
            Change::Grew(len) => {
                self.missing = false;
                self.raw_target = len;
                self.last_growth = Some(Instant::now());
                self.interval.on_activity();
                self.dirty = true;
            }
            Change::Truncated(len) => {
                self.missing = false;
                self.reset_pipeline();
                self.raw_target = len;
                self.last_growth = Some(Instant::now());
                self.interval.on_activity();
                let (generation, at) = self.note(NoticeKind::Truncated);
                self.shared.emit(DocEvent::Truncated { generation, at });
            }
            Change::Rotated => {
                let reopened = self.file.as_ref().map(|f| f.reopen());
                match reopened {
                    Some(Ok(())) => {
                        self.missing = false;
                        self.reset_pipeline();
                        self.raw_target = self.raw.len().unwrap_or(0);
                        self.last_growth = Some(Instant::now());
                        self.interval.on_activity();
                        let (generation, at) = self.note(NoticeKind::Rotated);
                        self.shared.emit(DocEvent::Rotated { generation, at });
                    }
                    // The new file vanished again already; treat as removed.
                    _ => self.mark_removed(),
                }
            }
            Change::Removed => self.mark_removed(),
        }
        self.ensure_detected();
        if let Some(d) = &self.detected
            && d.is_passthrough()
            && self.view_len != self.raw_target
        {
            self.view_len = self.raw_target;
            self.shared.view_len.store(self.view_len, Ordering::Release);
            self.shared.emit(DocEvent::Grew {
                utf8_len: self.view_len,
            });
            self.dirty = true;
        }
        self.dirty |= self.writing() != self.shared.snapshot.read().writing;
        if self.error.is_some() && !self.stalled {
            self.error = None;
            self.dirty = true;
        }
    }

    fn mark_removed(&mut self) {
        if !self.missing {
            self.missing = true;
            let at = SystemTime::now();
            self.notice = Some(Notice {
                kind: NoticeKind::Removed,
                at,
            });
            self.dirty = true;
            self.shared.emit(DocEvent::Removed { at });
        }
        self.interval.on_idle();
    }

    fn note(&mut self, kind: NoticeKind) -> (u64, SystemTime) {
        let at = SystemTime::now();
        self.notice = Some(Notice { kind, at });
        self.dirty = true;
        (self.shared.generation.load(Ordering::Acquire), at)
    }

    /// Drops all derived state (index, cache, spool contents) and bumps the
    /// generation. Called on truncation, rotation and encoding changes.
    fn reset_pipeline(&mut self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.cache.clear();
        self.shared.index.write().reset();
        self.raw_pos = 0;
        self.raw_target = 0;
        self.view_len = 0;
        self.indexed = 0;
        self.shared.view_len.store(0, Ordering::Release);
        self.detected = None;
        self.spool = None;
        self.shared.view.switch(self.raw.clone());
        self.stalled = false;
        self.dirty = true;
    }

    /// Detects the encoding once the raw source has data, and (re)builds the
    /// view accordingly.
    fn ensure_detected(&mut self) {
        if self.detected.is_some() || self.raw_target == 0 {
            return;
        }
        let want = (self.raw_target as usize).min(SAMPLE_LEN);
        let mut sample = vec![0u8; want];
        let mut got = 0;
        while got < want {
            match self.raw.read_at(got as u64, &mut sample[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => {
                    self.fail(format!("cannot read file: {e}"));
                    return;
                }
            }
        }
        if got == 0 {
            return;
        }
        let d = detect(&sample[..got], self.choice);
        if d.is_passthrough() {
            self.spool = None;
            self.shared.view.switch(self.raw.clone());
        } else {
            match Spool::new(self.opts.spool_dir.as_deref(), d.encoding, d.line_ending) {
                Ok(s) => {
                    self.shared.view.switch(s.source());
                    self.spool = Some(s);
                }
                Err(e) => {
                    self.fail(format!("cannot create spool file: {e}"));
                    return;
                }
            }
            self.raw_pos = 0;
            self.view_len = 0;
            self.shared.view_len.store(0, Ordering::Release);
        }
        self.detected = Some(d);
        self.dirty = true;
    }

    fn fail(&mut self, msg: String) {
        tracing::warn!("{}: {msg}", self.shared.name);
        self.error = Some(msg.clone());
        self.stalled = true;
        self.dirty = true;
        self.shared.emit(DocEvent::Error(msg));
    }

    /// One unit of background work: transcode a chunk, else index a chunk.
    fn step(&mut self) {
        if self.spool.is_some() && self.raw_pos < self.raw_target {
            self.step_transcode();
        } else if self.indexed < self.view_len {
            self.step_index();
        }
    }

    fn step_transcode(&mut self) {
        let want = ((self.raw_target - self.raw_pos) as usize).min(SPOOL_STEP);
        self.buf.resize(want, 0);
        let mut got = 0;
        while got < want {
            match self
                .raw
                .read_at(self.raw_pos + got as u64, &mut self.buf[got..want])
            {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => {
                    self.fail(format!("read failed: {e}"));
                    return;
                }
            }
        }
        if got == 0 {
            // The raw file shrank under us; the next poll will notice.
            self.stalled = true;
            self.interval.on_activity();
            return;
        }
        let Some(spool) = self.spool.as_mut() else {
            return;
        };
        if let Err(e) = spool.append(&self.buf[..got]) {
            self.fail(format!("spool write failed: {e}"));
            return;
        }
        self.raw_pos += got as u64;
        self.view_len = spool.len();
        self.shared.view_len.store(self.view_len, Ordering::Release);
        self.dirty = true;
        self.shared.emit(DocEvent::Grew {
            utf8_len: self.view_len,
        });
    }

    fn step_index(&mut self) {
        let mut sc: Scanner = self.shared.index.read().scanner();
        let start = sc.position();
        let end = self.view_len.min(start + INDEX_STEP);
        let mut cps = Vec::new();
        self.buf
            .resize(SCAN_CHUNK.min((end - start) as usize).max(1), 0);
        let mut short = false;
        while sc.position() < end {
            let want = ((end - sc.position()) as usize).min(self.buf.len());
            let mut got = 0;
            while got < want {
                match self
                    .shared
                    .view
                    .read_at(sc.position() + got as u64, &mut self.buf[got..want])
                {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) => {
                        // Commit what we have, then report.
                        sc.feed(&self.buf[..got], &mut cps);
                        self.shared.index.write().commit(&sc, cps);
                        self.indexed = self.shared.index.read().indexed_bytes();
                        self.fail(format!("indexing failed: {e}"));
                        return;
                    }
                }
            }
            sc.feed(&self.buf[..got], &mut cps);
            if got < want {
                short = true;
                break;
            }
        }
        let mut idx = self.shared.index.write();
        idx.commit(&sc, cps);
        self.indexed = idx.indexed_bytes();
        drop(idx);
        self.dirty = true;
        if short {
            // Source shorter than we thought: wait for the next poll.
            self.stalled = true;
            self.interval.on_activity();
        }
        let done = self.indexed >= self.view_len;
        if done || self.last_progress.elapsed() >= PROGRESS_GAP {
            self.last_progress = Instant::now();
            self.shared.emit(DocEvent::IndexProgress {
                indexed_bytes: self.indexed,
                total_bytes: self.view_len,
            });
        }
    }

    fn writing(&self) -> bool {
        self.source_open
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Acquire))
            || self
                .last_growth
                .is_some_and(|t| t.elapsed() < WRITING_WINDOW)
    }

    fn maybe_publish(&mut self, force: bool) {
        if self.dirty && (force || self.last_publish.elapsed() >= PROGRESS_GAP) {
            self.publish();
        }
    }

    fn publish(&mut self) {
        self.dirty = false;
        self.last_publish = Instant::now();
        let (known, indexed, estimated) = {
            let idx = self.shared.index.read();
            (
                idx.total_lines(),
                idx.indexed_bytes(),
                idx.estimate_total_lines(self.view_len),
            )
        };
        let exact = indexed >= self.view_len;
        let state = if let Some(e) = &self.error {
            DocState::Error(e.clone())
        } else if !self.initialised {
            DocState::Opening
        } else if self.view_len.saturating_sub(indexed) > INDEXING_BACKLOG {
            DocState::Indexing {
                fraction: indexed as f64 / self.view_len.max(1) as f64,
            }
        } else {
            DocState::Ready
        };
        let (encoding, line_ending) = match &self.detected {
            Some(d) => (d.encoding, d.line_ending),
            None => (
                match self.choice {
                    EncodingChoice::Fixed(e) => e,
                    EncodingChoice::Auto => TextEncoding::UTF_8,
                },
                LineEnding::Lf,
            ),
        };
        let snap = DocSnapshot {
            display_name: self.shared.name.clone(),
            path: self.shared.path.clone(),
            encoding,
            line_ending,
            spooled: self.spool.is_some(),
            utf8_len: self.view_len,
            indexed_bytes: indexed,
            lines: LineCount {
                known,
                exact,
                estimated_total: if exact { known } else { estimated },
            },
            state,
            generation: self.shared.generation.load(Ordering::Acquire),
            last_event: self.notice,
            writing: self.writing(),
            file_missing: self.missing,
        };
        *self.shared.snapshot.write() = Arc::new(snap);
    }
}
