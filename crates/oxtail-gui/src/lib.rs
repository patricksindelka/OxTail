//! egui front end for OxTail.
//!
//! The UI thread never does I/O and never blocks: files are opened on worker
//! threads, lines come from `Document` actors through channels (`try_recv`
//! each frame, with the document's waker calling `request_repaint`), and
//! searches, filters, config writes and alerts run on their own threads.
//! Logic that can be expressed without a `Ui` (viewport maths, key mapping,
//! caches, minimap binning, rule adapters, ...) lives in plain modules with
//! unit tests.

#![forbid(unsafe_code)]

pub mod alerts;
pub mod app;
pub mod colors;
pub mod docview;
pub mod filter;
pub mod find;
pub mod goto;
pub mod highlight;
pub mod icon;
pub mod keymap;
pub mod linecache;
pub mod logview;
pub mod minimap;
pub mod panels;
pub mod persist;
pub mod request;
pub mod ruleeditor;
pub mod rules;
pub mod scroll;
pub mod startup;
pub mod text;
pub mod util;
pub mod viewport;

pub use app::{OxTailApp, native_options};
pub use request::OpenRequest;
pub use startup::{AppInit, ExternalOpen, Startup, load_startup};
