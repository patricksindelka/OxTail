//! egui front end for OxTail.
//!
//! The UI thread never does I/O and never blocks: files are opened on worker
//! threads, lines come from `Document` actors through channels (`try_recv`
//! each frame, with the document's waker calling `request_repaint`), and
//! searches, filters, config writes and alerts run on their own threads.
//! Logic that can be expressed without a `Ui` (viewport maths, key mapping,
//! caches, minimap binning, rule adapters, ...) lives in plain modules with
//! unit tests.
//!
//! # Module map
//!
//! | Module | What |
//! |---|---|
//! | [`app`], [`appui`] | The application object, actions, session and settings plumbing; menus, tabs and dialogs |
//! | [`tab`] | Tabs and the session snapshot |
//! | [`docview`] | Per-document view state: position, follow, reads, selection, bookmarks |
//! | [`scroll`], [`linecache`], [`viewport`] | Row walking by line offset, the line cache, scrollbar maths |
//! | [`logview`], [`text`], [`colors`] | Painting the rows, laying out highlighted text, themes |
//! | [`find`], [`filter`], [`minimap`], [`goto`] | Search, filter views, minimap binning, go to line |
//! | [`highlight`], [`rules`], [`ruleeditor`] | Highlight state, profile rule adapter, the rule editor |
//! | [`alerts`], [`persist`] | Notification worker, configuration I/O worker |
//! | [`keymap`], [`panels`], [`request`], [`startup`], [`icon`], [`util`] | Shortcuts, bars, open requests, startup data, window icon, formatting |
//!
//! # Row identity
//!
//! Lines are identified by the byte offset of their first byte, not by line
//! number: numbers are only estimates while a big file is still being indexed,
//! offsets never are. The next line starts at `offset + len`, so the view can
//! walk and scroll before any index exists (see [`scroll`]).
//!
//! The columns and time crates are dependencies, but their UI (a table view,
//! go to time, the merged view) is a later task; the row renderer in
//! [`logview`] is where a table would replace the single text column.

#![forbid(unsafe_code)]

pub mod alerts;
pub mod app;
pub mod appui;
pub mod chooser;
pub mod collayout;
pub mod colors;
pub mod colspec;
pub mod colui;
pub mod detail;
pub mod docscan;
pub mod docview;
pub mod export;
pub mod filter;
pub mod find;
pub mod goto;
pub mod guistate;
pub mod highlight;
pub mod icon;
pub mod keymap;
pub mod linecache;
pub mod logview;
pub mod minimap;
pub mod panels;
pub mod panes;
pub mod persist;
pub mod qfilter;
pub mod request;
pub mod ruleeditor;
pub mod rules;
pub mod scroll;
pub mod startup;
pub mod stats;
pub mod structure;
pub mod tab;
pub mod table;
pub mod tablepaint;
pub mod text;
pub mod util;
pub mod viewport;

pub use app::{OxTailApp, native_options};
pub use request::OpenRequest;
pub use startup::{AppInit, ExternalOpen, Startup, load_startup};
