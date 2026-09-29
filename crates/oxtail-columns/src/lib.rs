//! Column parsers, auto-detection and query language for OxTail.
//!
//! Everything here works on `&str` lines and is GUI-free and I/O-free:
//!
//! * [`ParserSpec`] / [`Parser`]: delimited, regex, JSON Lines, logfmt,
//!   syslog, Apache/Nginx, W3C/IIS, log4j patterns and fixed width.
//! * [`detect`]: pick the best parser for a sample of lines.
//! * [`RecordGrouper`]: attach continuation lines (stack traces) to records.
//! * [`Query`]: the small typed filter language from PLAN.md section 8.4.
//! * [`TableStats`] / [`ColumnStats`]: bounded-memory column statistics.
//! * [`export_csv`] / [`export_jsonl`]: streaming export.
//! * [`format`] helpers and [`Level`] normalisation.
//!
//! Nothing here panics on arbitrary input; malformed lines yield `None`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod detect;
mod error;
mod export;
pub mod format;
mod group;
mod level;
mod model;
mod parsers;
mod quantity;
mod query;
mod stats;
mod timestamp;

pub use detect::{Detection, detect};
pub use error::ColumnsError;
pub use export::{export_csv, export_jsonl};
pub use group::RecordGrouper;
pub use level::{Level, normalize_level};
pub use model::{ColumnInfo, ColumnKind, Record, Schema};
pub use parsers::{
    FixedColumn, MAX_PARSE_LEN, Parser, ParserSpec, discover_json_columns, discover_logfmt_columns,
    log4j_pattern_to_regex,
};
pub use quantity::{Quantity, Unit, parse_bytes, parse_duration_ns, parse_quantity};
pub use query::{Query, QueryError};
pub use stats::{ColumnStats, Percentiles, TableStats, TopValue};
pub use timestamp::{Rfc3339Fallback, TimestampParser};
