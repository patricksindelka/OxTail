//! Search engine and filter views for OxTail (PLAN.md section 6).
//!
//! # Byte model
//!
//! Everything here works on UTF-8 bytes with `\n`-terminated lines; a `\r`
//! right before the `\n` is not part of the line. Lines are identified by the
//! **byte offset of their first byte**. Mapping offsets to line numbers is the
//! line index's job (`oxtail-core`), so every result is a line-start offset.
//!
//! # Pieces
//!
//! * [`Query`] / [`Matcher`]: compile a user query (literal or regex, smart
//!   case, whole word) into a fast matcher. Literals use `memchr::memmem`,
//!   many literals and ASCII case-insensitive literals use `aho-corasick`,
//!   regexes use the `regex` meta engine (which extracts required literals and
//!   uses SIMD prefilters). Whole chunks are scanned at once; candidates are
//!   verified on their own line so nothing ever matches across a `\n`.
//! * [`LinePredicate`]: the plug point shared by searches, filters and column
//!   queries.
//! * [`SearchJob`] / [`FilterJob`]: chunked parallel scans over a
//!   [`oxtail_core::ReadAt`] source, viewport-first, streaming, cancellable
//!   and extendable while a file is followed. Both return a [`SearchHandle`].
//! * [`MatchSet`]: the sorted set of line-start offsets that comes out.
//! * [`FilterStack`]: stacked include/exclude filters with context lines.

#![forbid(unsafe_code)]

mod job;
mod matcher;
mod matchset;
mod predicate;
mod scan;

pub use job::{
    DEFAULT_CHUNK_SIZE, FilterJob, Lookup, SearchHandle, SearchJob, SearchOptions, SearchStatus,
};
pub use matcher::{CaseMode, Matcher, Query, QueryKind, SearchError};
pub use matchset::MatchSet;
pub use predicate::{Filter, FilterMode, FilterStack, LinePredicate};

/// A filter job's handle; it is the same type as a search job's.
pub type FilterHandle = SearchHandle;
