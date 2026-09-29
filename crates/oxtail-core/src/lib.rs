//! File access, block cache, sparse line index, encodings and follow for OxTail.
//!
//! # Byte model
//!
//! Everything downstream of this crate (search, highlight, columns, GUI) sees
//! **UTF-8 bytes with `\n`-terminated lines**. Files in other encodings are
//! transcoded by this crate into a spool (see PLAN.md §5.6), so byte offsets
//! handed out by this crate always refer to the UTF-8 view, not necessarily
//! to the file on disk. A `\r` immediately before `\n` belongs to the line
//! terminator and is not part of the line's text.
//!
//! Line numbers are 0-based `u64`; line `n` starts right after the `n`-th
//! `\n` (line 0 starts at offset 0). A final line without a trailing `\n` is
//! a line too (it may still be growing while following).

pub mod cache;
pub mod error;
pub mod index;
pub mod source;

pub use error::CoreError;
pub use source::{MemSource, ReadAt};
