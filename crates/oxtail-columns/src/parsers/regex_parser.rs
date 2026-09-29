//! Regex-with-named-groups parser (also the engine for syslog, access logs
//! and log4j patterns).

use std::collections::BTreeMap;

use regex::{Regex, RegexBuilder};

use crate::error::ColumnsError;
use crate::model::{ColumnKind, Record, Schema};

#[derive(Debug, Clone)]
pub(crate) struct RegexImp {
    re: Regex,
    /// Capture-group index for each column.
    groups: Vec<usize>,
    names: Vec<String>,
}

impl RegexImp {
    pub(crate) fn from_pattern(pattern: &str) -> Result<Self, ColumnsError> {
        let re = RegexBuilder::new(pattern)
            .size_limit(8 << 20)
            .nest_limit(64)
            .build()?;
        let imp = Self::from_regex(re);
        if imp.names.is_empty() {
            return Err(ColumnsError::InvalidSpec(
                "regex has no named groups; use (?P<name>...) to define columns".into(),
            ));
        }
        Ok(imp)
    }

    pub(crate) fn from_regex(re: Regex) -> Self {
        let mut groups = Vec::new();
        let mut names = Vec::new();
        for (i, n) in re.capture_names().enumerate() {
            if let Some(n) = n {
                groups.push(i);
                names.push(n.to_string());
            }
        }
        RegexImp { re, groups, names }
    }

    pub(crate) fn schema(&self, kinds: &BTreeMap<String, ColumnKind>) -> Schema {
        Schema::from_names(&self.names, kinds)
    }

    pub(crate) fn is_match(&self, line: &str) -> bool {
        self.re.is_match(line)
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        let caps = self.re.captures(line)?;
        let mut rec = Record::with_columns(n);
        for (i, &g) in self.groups.iter().enumerate() {
            if let Some(m) = caps.get(g) {
                if !m.as_str().is_empty() {
                    rec.set_slice(i, line, m.range());
                }
            }
        }
        Some(rec)
    }
}
