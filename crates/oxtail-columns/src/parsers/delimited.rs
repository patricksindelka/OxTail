//! Delimited values (CSV/TSV/...) and W3C extended logs.

use std::cell::RefCell;
use std::ops::Range;

use csv_core::{ReadFieldResult, ReaderBuilder, Terminator};

use crate::model::Record;

/// One split field: a verbatim slice of the line or a decoded (unquoted) value.
pub(crate) enum Cell {
    Slice(Range<usize>),
    Owned(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Delimited {
    delim: u8,
    quote: u8,
    columns: Vec<String>,
    has_header: bool,
}

thread_local! {
    /// Built readers per `(delimiter, quote)`. Building a `csv_core::Reader`
    /// constructs a DFA (~10 microseconds), far too slow to do per line, and
    /// `Reader::clone` does not preserve the DFA's output table, so readers
    /// are cached per thread and `reset()` before each use.
    static READERS: RefCell<Vec<((u8, u8), csv_core::Reader)>> = const { RefCell::new(Vec::new()) };
}

fn with_reader<R>(delim: u8, quote: u8, f: impl FnOnce(&mut csv_core::Reader) -> R) -> R {
    READERS.with(|cell| {
        let mut v = cell.borrow_mut();
        let i = match v.iter().position(|(k, _)| *k == (delim, quote)) {
            Some(i) => i,
            None => {
                let r = ReaderBuilder::new()
                    .delimiter(delim)
                    .quote(quote)
                    .double_quote(true)
                    .terminator(Terminator::Any(b'\n'))
                    .build();
                v.push(((delim, quote), r));
                v.len() - 1
            }
        };
        let rdr = &mut v[i].1;
        rdr.reset();
        f(rdr)
    })
}

impl Delimited {
    pub(crate) fn new(delim: char, quote: char, columns: Vec<String>, has_header: bool) -> Self {
        let delim = if delim.is_ascii() { delim as u8 } else { b',' };
        let quote = if quote.is_ascii() { quote as u8 } else { b'"' };
        Delimited {
            delim,
            quote,
            columns,
            has_header,
        }
    }

    /// Splits a line into cells. `max` bounds the number of cells produced
    /// (extra cells are dropped but still counted via the returned total).
    pub(crate) fn split(&self, line: &str, max: usize) -> (Vec<Cell>, usize) {
        let bytes = line.as_bytes();
        let mut cells = Vec::with_capacity(max.min(64));
        let mut total = 0usize;
        if memchr::memchr(self.quote, bytes).is_none() {
            let mut start = 0;
            for pos in memchr::memchr_iter(self.delim, bytes) {
                total += 1;
                if cells.len() < max {
                    cells.push(Cell::Slice(start..pos));
                }
                start = pos + 1;
            }
            total += 1;
            if cells.len() < max {
                cells.push(Cell::Slice(start..bytes.len()));
            }
            return (cells, total);
        }
        if bytes.is_empty() {
            return (cells, 0);
        }
        with_reader(self.delim, self.quote, |rdr| {
        let mut out = vec![0u8; bytes.len() + 1];
        let mut pos = 0usize;
        let mut fstart = 0usize;
        // Output written so far for the current field (csv-core emits partial
        // output on `InputEmpty`, so it must be accumulated).
        let mut opos = 0usize;
        loop {
            let (res, nin, nout) = rdr.read_field(&bytes[pos..], &mut out[opos..]);
            pos += nin;
            opos += nout;
            match res {
                ReadFieldResult::InputEmpty => continue,
                ReadFieldResult::OutputFull => break,
                ReadFieldResult::End => break,
                ReadFieldResult::Field { record_end } => {
                    total += 1;
                    if cells.len() < max {
                        let raw = &bytes[fstart..pos];
                        let raw_no_delim = match raw.last() {
                            Some(&b) if b == self.delim => &raw[..raw.len() - 1],
                            _ => raw,
                        };
                        if memchr::memchr(self.quote, raw).is_none() && &out[..opos] == raw_no_delim
                        {
                            cells.push(Cell::Slice(fstart..fstart + opos));
                        } else {
                            cells.push(Cell::Owned(
                                String::from_utf8_lossy(&out[..opos]).into_owned(),
                            ));
                        }
                    }
                    fstart = pos;
                    opos = 0;
                    if record_end {
                        break;
                    }
                }
            }
        }
        });
        (cells, total)
    }

    /// Splits into owned strings (used for header lines).
    pub(crate) fn split_owned(&self, line: &str) -> Vec<String> {
        let (cells, _) = self.split(line, usize::MAX);
        cells
            .into_iter()
            .map(|c| match c {
                Cell::Slice(r) => line[r].to_string(),
                Cell::Owned(s) => s,
            })
            .collect()
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        let (cells, total) = self.split(line, n);
        if total != n || cells.len() != n {
            return None;
        }
        let mut rec = Record::with_columns(n);
        let mut same_as_header = self.has_header;
        for (i, cell) in cells.into_iter().enumerate() {
            match cell {
                Cell::Slice(r) => {
                    if same_as_header && line[r.clone()] != self.columns[i] {
                        same_as_header = false;
                    }
                    if !r.is_empty() {
                        rec.set_slice(i, line, r);
                    }
                }
                Cell::Owned(s) => {
                    if same_as_header && s != self.columns[i] {
                        same_as_header = false;
                    }
                    if !s.is_empty() {
                        rec.set_owned(i, s);
                    }
                }
            }
        }
        if same_as_header {
            return None;
        }
        Some(rec)
    }
}

/// W3C Extended / IIS: space (or tab) separated, `-` means "no value",
/// `#` lines are directives, not records.
#[derive(Debug, Clone)]
pub(crate) struct W3c {
    ncols: usize,
}

impl W3c {
    pub(crate) fn new(ncols: usize) -> Self {
        W3c { ncols }
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        if line.is_empty() || line.starts_with('#') || n != self.ncols {
            return None;
        }
        let delim = if memchr::memchr(b'\t', line.as_bytes()).is_some() {
            b'\t'
        } else {
            b' '
        };
        let mut rec = Record::with_columns(n);
        let mut start = 0;
        let mut count = 0;
        let bytes = line.as_bytes();
        let mut push = |start: usize, end: usize, count: usize| -> bool {
            if count >= n {
                return false;
            }
            if end > start && &line[start..end] != "-" {
                rec.set_slice(count, line, start..end);
            }
            true
        };
        for pos in memchr::memchr_iter(delim, bytes) {
            if !push(start, pos, count) {
                return None;
            }
            count += 1;
            start = pos + 1;
        }
        if !push(start, bytes.len(), count) {
            return None;
        }
        count += 1;
        (count == n).then_some(rec)
    }
}
