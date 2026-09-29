//! Parser definitions ([`ParserSpec`]) and their compiled form ([`Parser`]).
//!
//! A [`ParserSpec`] is plain serializable data (it lives in TOML profiles).
//! [`ParserSpec::compile`] turns it into a [`Parser`] that maps a `&str` line
//! to a [`Record`]. Parsers never panic on arbitrary input: a line that does
//! not fit the format simply yields `None`.

mod access;
mod builtin;
pub(crate) mod delimited;
mod fixed;
mod json;
mod log4j;
pub(crate) mod logfmt;
mod regex_parser;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ColumnsError;
use crate::model::{ColumnKind, Record, Schema};

pub use json::{discover_json_columns, discover_logfmt_columns};
pub use log4j::log4j_pattern_to_regex;

/// Lines longer than this are not parsed (they yield `None`).
pub const MAX_PARSE_LEN: usize = 1 << 20;

fn default_comma() -> char {
    ','
}

fn default_quote() -> char {
    '"'
}

/// One column of a fixed-width layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedColumn {
    /// Column name.
    pub name: String,
    /// First character position (0-based, in characters not bytes).
    pub start: usize,
    /// One past the last character position; `None` means "to end of line".
    #[serde(default)]
    pub end: Option<usize>,
}

/// A serializable parser definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParserSpec {
    /// CSV, TSV, `|`- or `;`-separated values. Quoting via `csv-core`.
    Delimited {
        /// Field delimiter (ASCII).
        #[serde(default = "default_comma")]
        delimiter: char,
        /// Quote character (ASCII).
        #[serde(default = "default_quote")]
        quote: char,
        /// True when the first line of the file is a header row.
        #[serde(default)]
        has_header: bool,
        /// Column names.
        columns: Vec<String>,
        /// Kind overrides by column name (otherwise guessed from the name).
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// A regular expression whose named groups become columns.
    Regex {
        /// The pattern (Rust `regex` syntax, `(?P<name>...)` groups).
        pattern: String,
        /// Kind overrides by column name.
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// One JSON object per line; nested keys are flattened with dots.
    JsonLines {
        /// Known keys, in display order. Other keys land in `Record::extra`.
        #[serde(default)]
        columns: Vec<String>,
        /// Kind overrides by column name.
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// `key=value key2="quoted value"` pairs.
    Logfmt {
        /// Known keys, in display order. Other keys land in `Record::extra`.
        #[serde(default)]
        columns: Vec<String>,
        /// Kind overrides by column name.
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// Syslog RFC 3164 (BSD): `<34>Oct 11 22:14:15 host tag[pid]: msg`.
    Syslog3164,
    /// Syslog RFC 5424: `<165>1 2026-09-29T10:00:00Z host app pid msgid sd msg`.
    Syslog5424,
    /// Apache / Nginx common log format.
    AccessCommon,
    /// Apache / Nginx combined log format.
    AccessCombined,
    /// W3C Extended / IIS logs; `fields` comes from the `#Fields:` directive.
    W3c {
        /// Field names in order.
        fields: Vec<String>,
        /// Kind overrides by column name.
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// A log4j/logback layout pattern such as `%d{ISO8601} %-5p [%t] %c{1} - %m%n`.
    Log4j {
        /// The layout pattern.
        pattern: String,
        /// Kind overrides by column name.
        #[serde(default)]
        kinds: BTreeMap<String, ColumnKind>,
    },
    /// Fixed-width columns.
    FixedWidth {
        /// The column layout.
        columns: Vec<FixedColumn>,
    },
}

impl ParserSpec {
    /// A comma-delimited spec with columns taken from a header line.
    ///
    /// Fields are split with the same quoting rules as data lines. Empty header
    /// cells become `col1`, `col2`...
    pub fn delimited_from_header_line(line: &str, delimiter: char) -> ParserSpec {
        let p = delimited::Delimited::new(delimiter, '"', Vec::new(), false);
        let names: Vec<String> = p
            .split_owned(line.strip_suffix('\r').unwrap_or(line))
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let n = n.trim().to_string();
                if n.is_empty() {
                    format!("col{}", i + 1)
                } else {
                    n
                }
            })
            .collect();
        ParserSpec::Delimited {
            delimiter,
            quote: '"',
            has_header: true,
            columns: names,
            kinds: BTreeMap::new(),
        }
    }

    /// A W3C spec from a `#Fields: date time c-ip ...` directive line.
    /// Returns `None` if the line is not a `#Fields:` directive.
    pub fn w3c_from_fields_line(line: &str) -> Option<ParserSpec> {
        let rest = line.trim().strip_prefix("#Fields:")?;
        let fields: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        if fields.is_empty() {
            return None;
        }
        Some(ParserSpec::W3c {
            fields,
            kinds: BTreeMap::new(),
        })
    }

    /// Compiles the spec into a [`Parser`].
    pub fn compile(&self) -> Result<Parser, ColumnsError> {
        let (schema, imp) = match self {
            ParserSpec::Delimited {
                delimiter,
                quote,
                has_header,
                columns,
                kinds,
            } => {
                if columns.is_empty() {
                    return Err(ColumnsError::InvalidSpec(
                        "delimited parser needs at least one column".into(),
                    ));
                }
                if !delimiter.is_ascii() || !quote.is_ascii() {
                    return Err(ColumnsError::InvalidSpec(
                        "delimiter and quote must be ASCII characters".into(),
                    ));
                }
                if delimiter == quote {
                    return Err(ColumnsError::InvalidSpec(
                        "delimiter and quote must differ".into(),
                    ));
                }
                let schema = Schema::from_names(columns, kinds);
                (
                    schema,
                    Imp::Delimited(delimited::Delimited::new(
                        *delimiter,
                        *quote,
                        columns.clone(),
                        *has_header,
                    )),
                )
            }
            ParserSpec::Regex { pattern, kinds } => {
                let imp = regex_parser::RegexImp::from_pattern(pattern)?;
                (imp.schema(kinds), Imp::Regex(imp))
            }
            ParserSpec::JsonLines { columns, kinds } => (
                Schema::from_names(columns, kinds),
                Imp::Json(json::JsonImp::new(columns)),
            ),
            ParserSpec::Logfmt { columns, kinds } => (
                Schema::from_names(columns, kinds),
                Imp::Logfmt(logfmt::LogfmtImp::new(columns)),
            ),
            ParserSpec::Syslog3164 => {
                let imp = regex_parser::RegexImp::from_regex(builtin::syslog3164().clone());
                (imp.schema(&BTreeMap::new()), Imp::Regex(imp))
            }
            ParserSpec::Syslog5424 => {
                let imp = regex_parser::RegexImp::from_regex(builtin::syslog5424().clone());
                (imp.schema(&BTreeMap::new()), Imp::Regex(imp))
            }
            ParserSpec::AccessCommon => (
                Schema::from_names(&access::COLUMNS, &BTreeMap::new()),
                Imp::Access(access::Access::new(false)),
            ),
            ParserSpec::AccessCombined => (
                Schema::from_names(&access::COLUMNS, &BTreeMap::new()),
                Imp::Access(access::Access::new(true)),
            ),
            ParserSpec::W3c { fields, kinds } => {
                if fields.is_empty() {
                    return Err(ColumnsError::InvalidSpec(
                        "W3C parser needs at least one field".into(),
                    ));
                }
                (
                    Schema::from_names(fields, kinds),
                    Imp::W3c(delimited::W3c::new(fields.len())),
                )
            }
            ParserSpec::Log4j { pattern, kinds } => {
                let re = log4j::log4j_pattern_to_regex(pattern)?;
                let imp = regex_parser::RegexImp::from_pattern(&re)?;
                (imp.schema(kinds), Imp::Regex(imp))
            }
            ParserSpec::FixedWidth { columns } => {
                if columns.is_empty() {
                    return Err(ColumnsError::InvalidSpec(
                        "fixed-width parser needs at least one column".into(),
                    ));
                }
                if let Some(c) = columns.iter().find(|c| c.end.is_some_and(|e| e <= c.start)) {
                    return Err(ColumnsError::InvalidSpec(format!(
                        "column '{}' ends before it starts",
                        c.name
                    )));
                }
                let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
                (
                    Schema::from_names(&names, &BTreeMap::new()),
                    Imp::Fixed(fixed::Fixed::new(columns)),
                )
            }
        };
        Ok(Parser {
            spec: self.clone(),
            schema,
            imp,
        })
    }

    /// A short human-readable name for UI display.
    pub fn display_name(&self) -> &'static str {
        match self {
            ParserSpec::Delimited { delimiter, .. } => match delimiter {
                ',' => "CSV",
                '\t' => "TSV",
                '|' => "Pipe-delimited",
                ';' => "Semicolon-delimited",
                _ => "Delimited",
            },
            ParserSpec::Regex { .. } => "Regex",
            ParserSpec::JsonLines { .. } => "JSON Lines",
            ParserSpec::Logfmt { .. } => "logfmt",
            ParserSpec::Syslog3164 => "Syslog (RFC 3164)",
            ParserSpec::Syslog5424 => "Syslog (RFC 5424)",
            ParserSpec::AccessCommon => "Apache/Nginx common",
            ParserSpec::AccessCombined => "Apache/Nginx combined",
            ParserSpec::W3c { .. } => "W3C / IIS",
            ParserSpec::Log4j { .. } => "log4j pattern",
            ParserSpec::FixedWidth { .. } => "Fixed width",
        }
    }
}

#[derive(Debug, Clone)]
enum Imp {
    Delimited(delimited::Delimited),
    Regex(regex_parser::RegexImp),
    Access(access::Access),
    Json(json::JsonImp),
    Logfmt(logfmt::LogfmtImp),
    W3c(delimited::W3c),
    Fixed(fixed::Fixed),
}

/// A compiled parser. Cheap to clone; `Send + Sync`.
#[derive(Debug, Clone)]
pub struct Parser {
    spec: ParserSpec,
    schema: Schema,
    imp: Imp,
}

impl Parser {
    /// The definition this parser was compiled from.
    pub fn spec(&self) -> &ParserSpec {
        &self.spec
    }

    /// The columns this parser produces.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Parses one line (without its line terminator; a trailing `\r` is
    /// ignored). Returns `None` when the line is not a record of this format,
    /// for example a stack-trace continuation line, a directive or a header.
    pub fn parse<'a>(&self, line: &'a str) -> Option<Record<'a>> {
        if line.len() > MAX_PARSE_LEN {
            return None;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let n = self.schema.len();
        match &self.imp {
            Imp::Delimited(p) => p.parse(line, n),
            Imp::Regex(p) => p.parse(line, n),
            Imp::Access(p) => p.parse(line, n),
            Imp::Json(p) => p.parse(line, n),
            Imp::Logfmt(p) => p.parse(line, n),
            Imp::W3c(p) => p.parse(line, n),
            Imp::Fixed(p) => p.parse(line, n),
        }
    }

    /// Cheaper than [`Parser::parse`] when only "is this a record?" matters.
    pub fn is_match(&self, line: &str) -> bool {
        if line.len() > MAX_PARSE_LEN {
            return false;
        }
        // `parse` strips one trailing `\r` itself; strip here only for the
        // regex fast path so both agree (a double strip broke "\r\r" lines).
        match &self.imp {
            Imp::Regex(p) => p.is_match(line.strip_suffix('\r').unwrap_or(line)),
            _ => self.parse(line).is_some(),
        }
    }
}

#[cfg(test)]
mod tests;
