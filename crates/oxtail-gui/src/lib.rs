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
//! | [`actions`], [`palette`], [`settingsui`], [`sysint`], [`welcome`] | Every command as an action, the command palette, the settings window, system integration and update check, the start screen |
//! | [`keymap`], [`panels`], [`request`], [`startup`], [`icon`], [`util`] | Shortcuts, bars, open requests, startup data, window icon, formatting |
//! | [`structure`], [`colspec`], [`chooser`], [`guistate`] | Which parser a tab uses (profile, detection, choice), profile table adapters, the parser chooser, remembered choices |
//! | [`collayout`], [`table`], [`tablepaint`], [`colui`], [`detail`] | The column table: layout maths, header, cells, suggestion bar and windows, the detail pane |
//! | [`qfilter`], [`stats`], [`export`], [`docscan`] | Column queries as filters, statistics, CSV/JSON Lines export, the scanner they share |
//! | [`sort`] | Sorting a filtered view by a column: background job, sorted row order |
//! | [`timeview`], [`gototime`] | Relative time and gap separators, go to time |
//! | [`merge`], [`mergefilter`], [`mergeview`], [`mergepaint`], [`appmerge`] | Merged tabs: builder, filter worker, virtual list, painting, opening |
//! | [`panes`], [`appsplit`], [`panesui`], [`cross`] | Split layout tree, pane state, tab strips and pane drawing, search across tabs |
//!
//! # Row identity
//!
//! Lines are identified by the byte offset of their first byte, not by line
//! number: numbers are only estimates while a big file is still being indexed,
//! offsets never are. The next line starts at `offset + len`, so the view can
//! walk and scroll before any index exists (see [`scroll`]).
//!
//! # Columns, time and several files
//!
//! The table is another renderer of the same virtualized rows: [`logview`]
//! paints cells (from [`tablepaint`]) instead of one text galley when the tab
//! has a parser and the table is on. Lines are parsed only when they are
//! prepared for drawing, cached with their highlights by offset. A merged tab
//! has its own list ([`mergeview`]) because its rows are `(source, line)`
//! pairs, not offsets of one file. Every long job (structure decision,
//! statistics, export, go to time, merging, searching) is a worker thread
//! with a cancel flag that reports back through a channel and wakes the UI.
//!
//! GUI-side state that the session file has no room for (column choices per
//! file, pane layout, merged tabs) is kept in `gui-state.json` ([`guistate`]).

#![forbid(unsafe_code)]

pub mod actions;
pub mod alerts;
pub mod app;
pub mod appmerge;
pub mod appsplit;
pub mod appui;
pub mod chooser;
pub mod collayout;
pub mod colors;
pub mod colspec;
pub mod colui;
pub mod cross;
pub mod detail;
pub mod docscan;
pub mod docview;
pub mod export;
pub mod filter;
pub mod find;
pub mod goto;
pub mod gototime;
pub mod guistate;
pub mod highlight;
pub mod icon;
pub mod keymap;
pub mod linecache;
pub mod logview;
pub mod merge;
pub mod mergefilter;
pub mod mergepaint;
pub mod mergeview;
pub mod minimap;
pub mod palette;
pub mod panels;
pub mod panes;
pub mod panesui;
pub mod persist;
pub mod qfilter;
pub mod request;
pub mod ruleeditor;
pub mod rules;
pub mod scroll;
pub mod settingsui;
pub mod sort;
pub mod startup;
pub mod stats;
pub mod structure;
pub mod sysint;
pub mod tab;
pub mod table;
pub mod tablepaint;
pub mod text;
pub mod timeview;
pub mod util;
pub mod viewport;
pub mod welcome;

pub use app::{OxTailApp, native_options};
pub use request::OpenRequest;
pub use startup::{AppInit, ExternalOpen, Startup, load_startup};
