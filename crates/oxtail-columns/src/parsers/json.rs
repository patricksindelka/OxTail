//! JSON Lines parser and schema discovery.

use std::borrow::Cow;
use std::collections::HashMap;

use serde_json::Value;

use crate::model::Record;

/// Maximum number of columns discovered from samples.
pub(crate) const MAX_DISCOVERED_COLUMNS: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct JsonImp {
    index: HashMap<String, usize>,
}

impl JsonImp {
    pub(crate) fn new(columns: &[String]) -> Self {
        JsonImp {
            index: columns
                .iter()
                .enumerate()
                .map(|(i, c)| (c.clone(), i))
                .collect(),
        }
    }

    pub(crate) fn parse<'a>(&self, line: &'a str, n: usize) -> Option<Record<'a>> {
        let t = line.trim();
        if !t.starts_with('{') {
            return None;
        }
        let Value::Object(map) = serde_json::from_str::<Value>(t).ok()? else {
            return None;
        };
        let mut rec = Record::with_columns(n);
        let mut path = String::new();
        flatten_into(&map, &mut path, &mut |key, val| {
            if let Some(&i) = self.index.get(key) {
                if i < n && rec.fields[i].is_none() {
                    rec.fields[i] = Some(Cow::Owned(val));
                }
            } else {
                rec.extra
                    .push((Cow::Owned(key.to_string()), Cow::Owned(val)));
            }
        });
        Some(rec)
    }
}

/// Text form of a leaf JSON value; `None` for null.
fn leaf_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        // Arrays (and empty objects) are kept as compact JSON text.
        other => serde_json::to_string(other).ok(),
    }
}

/// Walks `map`, calling `f(dotted_key, text)` for each leaf. `path` is scratch
/// space holding the current key prefix.
pub(crate) fn flatten_into(
    map: &serde_json::Map<String, Value>,
    path: &mut String,
    f: &mut dyn FnMut(&str, String),
) {
    let base = path.len();
    for (k, v) in map {
        if base > 0 {
            path.push('.');
        }
        path.push_str(k);
        match v {
            Value::Object(m) if !m.is_empty() => flatten_into(m, path, f),
            other => {
                if let Some(t) = leaf_text(other) {
                    f(path, t);
                }
            }
        }
        path.truncate(base);
    }
}

fn order_key(name: &str) -> usize {
    match name.to_ascii_lowercase().as_str() {
        "ts" | "time" | "timestamp" | "@timestamp" | "datetime" => 0,
        "level" | "lvl" | "severity" | "loglevel" => 1,
        "msg" | "message" => 3,
        _ => 2,
    }
}

/// Orders discovered names: timestamp, level, other keys (first-seen order),
/// message last.
fn order_columns(mut names: Vec<String>) -> Vec<String> {
    // Stable sort keeps first-seen order inside each bucket.
    names.sort_by_key(|n| order_key(n));
    names
}

/// Discovers JSON Lines columns from sample lines: the union of (flattened)
/// keys in first-seen order, capped at 64, with well-known keys first
/// (timestamp, level) and the message last.
pub fn discover_json_columns<S: AsRef<str>>(sample: &[S]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut set = std::collections::HashSet::new();
    for line in sample {
        let t = line.as_ref().trim();
        if !t.starts_with('{') {
            continue;
        }
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(t) else {
            continue;
        };
        let mut path = String::new();
        flatten_into(&map, &mut path, &mut |k, _| {
            if seen.len() < MAX_DISCOVERED_COLUMNS && set.insert(k.to_string()) {
                seen.push(k.to_string());
            }
        });
    }
    order_columns(seen)
}

/// Discovers logfmt keys from sample lines (first-seen order, capped at 64,
/// well-known keys ordered first like [`discover_json_columns`]).
pub fn discover_logfmt_columns<S: AsRef<str>>(sample: &[S]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut set = std::collections::HashSet::new();
    for line in sample {
        super::logfmt::scan(line.as_ref(), |key, _, _| {
            if seen.len() < MAX_DISCOVERED_COLUMNS && set.insert(key.to_string()) {
                seen.push(key.to_string());
            }
        });
    }
    order_columns(seen)
}
