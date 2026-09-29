//! Settings, profiles, themes, session and portable data folder for OxTail
//! (PLAN.md §3.2 and §11).
//!
//! Everything here is plain data plus pure functions, apart from the explicit
//! load/save functions and [`ConfigWatcher`]. Nothing panics on bad files:
//! loaders return defaults plus a list of warnings.
//!
//! * [`DataDir`]: where configuration lives (CLI, portable, installed or
//!   in-memory) and [`write_atomic`].
//! * [`Settings`]: `settings.toml` with schema versioning and migration.
//! * [`Profile`] / [`ProfileSet`]: `profiles/*.toml`, built-ins, glob and
//!   content based selection. See the rule shape below.
//! * [`Theme`] / [`ThemeSet`]: `themes/*.toml` and the built-in themes.
//! * [`Session`]: `session.json` with paths relative to the executable.
//! * [`ConfigWatcher`]: debounced hot-reload notifications.
//! * [`integration`]: opt-in "Open with" / Start menu registration and its removal.
//! * [`update`]: the opt-in, throttled update check (no auto-download).
//!
//! # Profile file shape
//!
//! `columns`, `rules` and `timestamp` are kept as opaque TOML here; typed
//! forms live in `oxtail-columns` and `oxtail-highlight`, and the GUI
//! deserialises them.
//!
//! ```toml
//! name = "Go service (logfmt)"
//! match_files = ["*service*.log"]     # globs: * ** ? [abc]
//! match_content = '^time=\S+ level='  # regex tried on the first lines
//! default_filter = 'level != "debug"'
//!
//! [columns]
//! parser = "logfmt"                   # logfmt | jsonl | nginx_combined | ...
//! order  = ["time", "level", "msg", "*"]
//!
//! [timestamp]
//! column = "time"
//! format = "rfc3339"
//!
//! [[rules]]
//! name  = "errors"
//! match = { column = "level", op = "eq", value = "error" }
//! scope = "line"                      # line | match | group:1 | column:level
//! style = { fg = "error", bg = "error.subtle", bold = true }
//! priority = 0
//! alert = false
//! hide  = false
//! ```
//!
//! The `match` of a rule is one of `{ regex = "..." }`, `{ literal = "..." }`
//! or `{ column = "level", op = "eq", value = "error" }`. Colours are
//! semantic names resolved through the active [`Theme`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod atomic;
mod datadir;
mod error;
mod glob;
pub mod integration;
mod profile;
mod session;
mod settings;
mod theme;
pub mod update;
mod watcher;

pub use atomic::{TEMP_SUFFIX, write_atomic};
pub use datadir::{
    DataDir, DataMode, PORTABLE_DIR, PORTABLE_MARKER, ResolveInput, platform_config_dir,
};
pub use error::ConfigError;
pub use glob::Glob;
pub use profile::{Loaded, Profile, ProfileSet, builtin_stems};
pub use session::{
    Bookmark, LayoutNode, PathMapper, Session, SplitDirection, TabState, WindowGeometry,
};
pub use settings::{
    CURRENT_SCHEMA_VERSION, Keymap, Renderer, Settings, ThemeChoice, TimezoneSetting, migrate,
};
pub use theme::{Rgb, Theme, ThemeSet, UiColors};
pub use watcher::{ConfigChanged, ConfigKind, ConfigWatcher};
