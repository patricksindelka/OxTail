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
//!
//! # Where to start
//!
//! [`Document`] is the entry point: one actor thread per open file, a
//! non-blocking [`DocSnapshot`], requests in and [`DocEvent`]s out. The other
//! modules ([`source`], [`cache`], [`index`], [`encoding`], [`line`],
//! [`follow`]) are the building blocks it is made of and are usable on their own.

#![deny(missing_docs)]

pub mod cache;
pub mod document;
pub mod encoding;
pub mod error;
pub mod follow;
pub mod index;
pub mod line;
pub mod source;
mod spool;

pub use cache::{BLOCK_SIZE, BlockCache, CachedSource};
pub use document::{
    DocEvent, DocSnapshot, DocState, Document, LineCount, LinePosition, LineRequest, Notice,
    NoticeKind, OpenOptions, RequestId,
};
pub use encoding::{EncodingChoice, LineEnding, TextEncoding};
pub use error::CoreError;
pub use follow::FollowMode;
pub use index::LineIndex;
pub use line::Line;
pub use source::FileSource;
pub use source::{MemSource, ReadAt};
