//! Fixed-width columns.

use crate::model::Record;
use crate::parsers::FixedColumn;

#[derive(Debug, Clone)]
pub(crate) struct Fixed {
    cols: Vec<FixedColumn>,
    min_start: usize,
}

impl Fixed {
    pub(crate) fn new(cols: &[FixedColumn]) -> Self {
        Fixed {
            cols: cols.to_vec(),
            min_start: cols.iter().map(|c| c.start).min().unwrap_or(0),
        }
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        if n != self.cols.len() {
            return None;
        }
        // Map char positions to byte offsets (identity for ASCII lines).
        let ascii = line.is_ascii();
        let nchars = if ascii {
            line.len()
        } else {
            line.chars().count()
        };
        if nchars <= self.min_start {
            return None;
        }
        let offsets: Vec<usize> = if ascii {
            Vec::new()
        } else {
            line.char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(line.len()))
                .collect()
        };
        let byte_at = |c: usize| -> usize {
            let c = c.min(nchars);
            if ascii { c } else { offsets[c] }
        };
        let mut rec = Record::with_columns(n);
        for (i, col) in self.cols.iter().enumerate() {
            let s = byte_at(col.start);
            let e = byte_at(col.end.unwrap_or(nchars));
            if s >= e {
                continue;
            }
            let raw = &line[s..e];
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lead = raw.len() - raw.trim_start().len();
            rec.set_slice(i, line, s + lead..s + lead + trimmed.len());
        }
        Some(rec)
    }
}
