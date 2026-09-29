//! logfmt parser: `key=value key2="quoted \" value" bare`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use crate::model::Record;

#[derive(Debug, Clone)]
pub(crate) struct LogfmtImp {
    index: HashMap<String, usize>,
}

/// A logfmt value: verbatim slice, or decoded text (escapes / bare key).
pub(crate) enum Val<'a> {
    Slice(Range<usize>),
    Owned(Cow<'a, str>),
}

/// Scans `line`, calling `f(key, value, has_equals)` per token.
/// Returns the number of `key=value` pairs found.
pub(crate) fn scan<'a>(line: &'a str, mut f: impl FnMut(&'a str, Val<'a>, bool)) -> usize {
    let b = line.as_bytes();
    let mut i = 0;
    let mut pairs = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = &line[ks..i];
        if i < b.len() && b[i] == b'=' {
            i += 1;
            if i < b.len() && b[i] == b'"' {
                i += 1;
                let vs = i;
                let mut escaped = false;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        escaped = true;
                        i += 1;
                    }
                    i += 1;
                }
                let ve = i.min(b.len());
                if i < b.len() {
                    i += 1; // closing quote
                }
                if key.is_empty() {
                    continue;
                }
                pairs += 1;
                if escaped {
                    f(key, Val::Owned(Cow::Owned(unescape(&line[vs..ve]))), true);
                } else {
                    f(key, Val::Slice(vs..ve), true);
                }
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if key.is_empty() {
                    continue;
                }
                pairs += 1;
                f(key, Val::Slice(vs..i), true);
            }
        } else if !key.is_empty() {
            f(key, Val::Owned(Cow::Borrowed("true")), false);
        }
    }
    pairs
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl LogfmtImp {
    pub(crate) fn new(columns: &[String]) -> Self {
        LogfmtImp {
            index: columns
                .iter()
                .enumerate()
                .map(|(i, c)| (c.clone(), i))
                .collect(),
        }
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        let mut rec = Record::with_columns(n);
        let pairs = scan(line, |key, val, _| match self.index.get(key) {
            Some(&i) if i < n => {
                if rec.fields[i].is_none() {
                    match val {
                        Val::Slice(r) => {
                            if !r.is_empty() {
                                rec.set_slice(i, line, r);
                            }
                        }
                        Val::Owned(c) => rec.fields[i] = Some(c),
                    }
                }
            }
            _ => {
                let v = match val {
                    Val::Slice(r) => Cow::Borrowed(&line[r]),
                    Val::Owned(c) => c,
                };
                rec.extra.push((Cow::Borrowed(key), v));
            }
        });
        (pairs > 0).then_some(rec)
    }
}
