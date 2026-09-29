//! Core data model: schemas, column kinds and parsed records.

use std::borrow::Cow;
use std::ops::Range;

use serde::{Deserialize, Serialize};

/// What a column holds. Drives formatting, statistics and query semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    /// Free text.
    #[default]
    Text,
    /// A plain number.
    Number,
    /// A timestamp (interpreted by a `TimestampParser`).
    Timestamp,
    /// A duration; a bare number is read as milliseconds.
    Duration,
    /// A size; a bare number is read as bytes.
    Bytes,
    /// JSON text (objects/arrays).
    Json,
    /// A log level (`WARN`, `w`, `warning` all mean the same).
    Level,
}

impl ColumnKind {
    /// Guesses a kind from a column name (`ts`, `level`, `status`, `latency`, ...).
    pub fn guess(name: &str) -> ColumnKind {
        let n = name.to_ascii_lowercase();
        match n.as_str() {
            "ts" | "time" | "timestamp" | "@timestamp" | "datetime" | "asctime" | "date" => {
                ColumnKind::Timestamp
            }
            "level" | "lvl" | "severity" | "loglevel" | "log_level" | "levelname" => {
                ColumnKind::Level
            }
            "status" | "status_code" | "code" | "pid" | "tid" | "port" | "line" | "count"
            | "sc-status" | "http.status" => ColumnKind::Number,
            "size" | "bytes" | "length" | "content_length" | "body_bytes_sent" | "bytes_sent"
            | "sc-bytes" | "cs-bytes" => ColumnKind::Bytes,
            "duration" | "elapsed" | "latency" | "took" | "time_taken" | "time-taken"
            | "request_time" | "response_time" | "rt" | "duration_ms" => ColumnKind::Duration,
            _ => ColumnKind::Text,
        }
    }
}

/// One column of a [`Schema`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// Column name (unique within a schema).
    pub name: String,
    /// Column kind.
    #[serde(default)]
    pub kind: ColumnKind,
}

impl ColumnInfo {
    /// Creates a column with an explicit kind.
    pub fn new(name: impl Into<String>, kind: ColumnKind) -> Self {
        ColumnInfo {
            name: name.into(),
            kind,
        }
    }
}

/// Ordered list of columns that a parser produces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    /// The columns, in display order.
    pub columns: Vec<ColumnInfo>,
}

/// Well-known aliases: a query for `level` finds a column called `severity`.
const ALIASES: &[&[&str]] = &[
    &[
        "ts",
        "time",
        "timestamp",
        "@timestamp",
        "datetime",
        "asctime",
    ],
    &["level", "lvl", "severity", "loglevel", "levelname"],
    &["msg", "message"],
];

impl Schema {
    /// Builds a schema from names, guessing each kind (see [`ColumnKind::guess`])
    /// unless `kinds` overrides it. Duplicate names get a `_2`, `_3`... suffix.
    pub fn from_names<S: AsRef<str>>(
        names: &[S],
        kinds: &std::collections::BTreeMap<String, ColumnKind>,
    ) -> Schema {
        let mut columns: Vec<ColumnInfo> = Vec::with_capacity(names.len());
        for n in names {
            let base = n.as_ref();
            let mut name = base.to_string();
            let mut i = 2;
            while columns.iter().any(|c| c.name == name) {
                name = format!("{base}_{i}");
                i += 1;
            }
            let kind = kinds
                .get(base)
                .copied()
                .unwrap_or_else(|| ColumnKind::guess(base));
            columns.push(ColumnInfo { name, kind });
        }
        Schema { columns }
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// True when there are no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Finds a column by name: exact match, then case-insensitive, then via
    /// well-known aliases (`severity` for `level`, `message` for `msg`, ...).
    pub fn find(&self, name: &str) -> Option<usize> {
        if let Some(i) = self.columns.iter().position(|c| c.name == name) {
            return Some(i);
        }
        if let Some(i) = self
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
        {
            return Some(i);
        }
        let group = ALIASES
            .iter()
            .find(|g| g.iter().any(|a| a.eq_ignore_ascii_case(name)))?;
        self.columns
            .iter()
            .position(|c| group.iter().any(|a| a.eq_ignore_ascii_case(&c.name)))
    }
}

/// One parsed line (or multi-line record).
///
/// `fields` and `spans` are aligned with the parser's [`Schema`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record<'a> {
    /// Field values; `None` when absent or empty.
    pub fields: Vec<Option<Cow<'a, str>>>,
    /// Byte range within the source line when the value is a verbatim slice
    /// of it (used for per-column highlighting). `None` for decoded values.
    pub spans: Vec<Option<Range<usize>>>,
    /// Keys not present in the schema (JSON / logfmt), in encounter order.
    pub extra: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

impl<'a> Record<'a> {
    /// An empty record with `n` absent fields.
    pub fn with_columns(n: usize) -> Self {
        Record {
            fields: vec![None; n],
            spans: vec![None; n],
            extra: Vec::new(),
        }
    }

    /// The value of column `idx`, if present.
    pub fn get(&self, idx: usize) -> Option<&str> {
        self.fields.get(idx)?.as_deref()
    }

    /// The span of column `idx` within the source line, if it is a slice of it.
    pub fn span(&self, idx: usize) -> Option<Range<usize>> {
        self.spans.get(idx)?.clone()
    }

    /// Looks up an extra (non-schema) key, case-insensitively.
    pub fn extra_value(&self, key: &str) -> Option<&str> {
        self.extra
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_ref())
    }

    /// Copies all borrowed data so the record no longer borrows the line.
    pub fn into_owned(self) -> Record<'static> {
        Record {
            fields: self
                .fields
                .into_iter()
                .map(|f| f.map(|c| Cow::Owned(c.into_owned())))
                .collect(),
            spans: self.spans,
            extra: self
                .extra
                .into_iter()
                .map(|(k, v)| (Cow::Owned(k.into_owned()), Cow::Owned(v.into_owned())))
                .collect(),
        }
    }

    pub(crate) fn set_slice(&mut self, idx: usize, line: &'a str, range: Range<usize>) {
        if let Some(slot) = self.fields.get_mut(idx) {
            *slot = Some(Cow::Borrowed(&line[range.clone()]));
            self.spans[idx] = Some(range);
        }
    }

    pub(crate) fn set_owned(&mut self, idx: usize, value: String) {
        if let Some(slot) = self.fields.get_mut(idx) {
            *slot = Some(Cow::Owned(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_uses_aliases_and_case() {
        let s = Schema::from_names(&["Timestamp", "severity", "message"], &Default::default());
        assert_eq!(s.find("ts"), Some(0));
        assert_eq!(s.find("LEVEL"), Some(1));
        assert_eq!(s.find("msg"), Some(2));
        assert_eq!(s.find("nope"), None);
    }

    #[test]
    fn duplicate_names_are_suffixed() {
        let s = Schema::from_names(&["a", "a", "a"], &Default::default());
        let names: Vec<_> = s.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "a_2", "a_3"]);
    }

    #[test]
    fn kind_guessing() {
        assert_eq!(ColumnKind::guess("Status"), ColumnKind::Number);
        assert_eq!(ColumnKind::guess("latency"), ColumnKind::Duration);
        assert_eq!(ColumnKind::guess("foo"), ColumnKind::Text);
    }
}
