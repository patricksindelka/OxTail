//! Timestamp detection, go-to-time and merged views for OxTail (PLAN.md §9).
//!
//! * [`detect`] finds the first timestamp-like token in a line; [`TimeParser`]
//!   learns the dominant format from sample lines and then parses on a fast
//!   path, falling back to full detection.
//! * [`find_time`] implements "go to time" as a binary search over lines with a
//!   documented linear fallback.
//! * [`MergeBuilder`] merges several sources by timestamp (k-way merge) into
//!   compact [`MergedEntry`] values, incrementally while following.
//! * [`helpers`] has gap detection and time formatting.
//!
//! Zone-less timestamps use the zone in [`TimeContext`]; nothing in this crate
//! touches the file system, so everything is testable without I/O.

#![forbid(unsafe_code)]

mod detect;
mod find;
pub mod helpers;
mod merge;
mod parser;

pub use detect::{Detected, SCAN_LIMIT, TimeContext, TimestampFormat, detect};
pub use find::{Cancelled, LineAccess, find_time, find_time_cancellable};
pub use merge::{MAX_LINE, MAX_SOURCES, MergeBuilder, MergeConfig, MergedEntry};
pub use parser::TimeParser;

/// Re-export of jiff, whose types appear in this crate's API.
pub use jiff;
