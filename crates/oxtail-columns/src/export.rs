//! Streaming export of records to CSV and JSON Lines.

use std::borrow::Borrow;
use std::io::{self, Write};

use serde_json::{Map, Number, Value};

use crate::model::{ColumnKind, Record, Schema};

fn csv_field<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    let needs_quote = s.contains([',', '"', '\n', '\r'])
        || s.starts_with(char::is_whitespace)
        || s.ends_with(char::is_whitespace);
    if !needs_quote {
        return w.write_all(s.as_bytes());
    }
    w.write_all(b"\"")?;
    let mut rest = s;
    while let Some(i) = rest.find('"') {
        w.write_all(rest[..=i].as_bytes())?;
        w.write_all(b"\"")?;
        rest = &rest[i + 1..];
    }
    w.write_all(rest.as_bytes())?;
    w.write_all(b"\"")
}

/// Writes `records` as CSV: a header row of the selected column names, then one
/// row per record. `columns` are indices into `schema` (out-of-range indices
/// are skipped); fields are quoted per RFC 4180 and rows end with `\n`.
/// Returns the number of records written. The caller should pass a buffered
/// writer; nothing is buffered here, so memory use is constant.
pub fn export_csv<'a, W, I, R>(
    w: &mut W,
    schema: &Schema,
    columns: &[usize],
    records: I,
) -> io::Result<u64>
where
    W: Write,
    I: IntoIterator<Item = R>,
    R: Borrow<Record<'a>>,
{
    let cols: Vec<usize> = columns
        .iter()
        .copied()
        .filter(|&c| c < schema.len())
        .collect();
    for (n, &c) in cols.iter().enumerate() {
        if n > 0 {
            w.write_all(b",")?;
        }
        csv_field(w, &schema.columns[c].name)?;
    }
    w.write_all(b"\n")?;
    let mut count = 0;
    for rec in records {
        let rec = rec.borrow();
        for (n, &c) in cols.iter().enumerate() {
            if n > 0 {
                w.write_all(b",")?;
            }
            csv_field(w, rec.get(c).unwrap_or(""))?;
        }
        w.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

fn json_value(kind: ColumnKind, s: &str) -> Value {
    match kind {
        ColumnKind::Number => {
            if let Ok(n) = serde_json::from_str::<Number>(s) {
                return Value::Number(n);
            }
        }
        ColumnKind::Json => {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                return v;
            }
        }
        _ => {}
    }
    Value::String(s.to_string())
}

/// Writes `records` as JSON Lines, one object per record with the selected
/// columns in order. Absent values become `null`; number columns are emitted
/// as JSON numbers when they are valid ones and JSON columns are embedded as
/// JSON. Returns the number of records written.
pub fn export_jsonl<'a, W, I, R>(
    w: &mut W,
    schema: &Schema,
    columns: &[usize],
    records: I,
) -> io::Result<u64>
where
    W: Write,
    I: IntoIterator<Item = R>,
    R: Borrow<Record<'a>>,
{
    let cols: Vec<usize> = columns
        .iter()
        .copied()
        .filter(|&c| c < schema.len())
        .collect();
    let mut count = 0;
    for rec in records {
        let rec = rec.borrow();
        let mut obj = Map::new();
        for &c in &cols {
            let info = &schema.columns[c];
            let v = rec.get(c).map_or(Value::Null, |s| json_value(info.kind, s));
            obj.insert(info.name.clone(), v);
        }
        serde_json::to_writer(&mut *w, &Value::Object(obj)).map_err(io::Error::other)?;
        w.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ColumnInfo;

    fn schema() -> Schema {
        Schema {
            columns: vec![
                ColumnInfo::new("id", ColumnKind::Number),
                ColumnInfo::new("msg", ColumnKind::Text),
                ColumnInfo::new("data", ColumnKind::Json),
            ],
        }
    }

    fn rec(a: Option<&str>, b: Option<&str>, c: Option<&str>) -> Record<'static> {
        let mut r = Record::with_columns(3);
        for (i, v) in [a, b, c].into_iter().enumerate() {
            if let Some(v) = v {
                r.set_owned(i, v.to_string());
            }
        }
        r
    }

    #[test]
    fn csv_quoting() {
        let recs = vec![
            rec(Some("1"), Some("plain"), None),
            rec(Some("2"), Some("a, \"b\"\nc"), Some("[1,2]")),
        ];
        let mut out = Vec::new();
        let n = export_csv(&mut out, &schema(), &[1, 0, 9], &recs).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "msg,id\nplain,1\n\"a, \"\"b\"\"\nc\",2\n"
        );
    }

    #[test]
    fn jsonl_types() {
        let recs = vec![
            rec(Some("7"), Some("hi \"x\""), Some(r#"{"k":[1]}"#)),
            rec(Some("abc"), None, Some("not json")),
        ];
        let mut out = Vec::new();
        export_jsonl(&mut out, &schema(), &[0, 1, 2], recs.iter()).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], r#"{"id":7,"msg":"hi \"x\"","data":{"k":[1]}}"#);
        assert_eq!(lines[1], r#"{"id":"abc","msg":null,"data":"not json"}"#);
    }

    #[test]
    fn csv_output_parses_back() {
        use crate::parsers::ParserSpec;
        let recs = vec![rec(Some("1"), Some("x, \"y\""), Some("z"))];
        let mut out = Vec::new();
        export_csv(&mut out, &schema(), &[0, 1, 2], &recs).unwrap();
        let text = String::from_utf8(out).unwrap();
        let mut lines = text.lines();
        let p = ParserSpec::delimited_from_header_line(lines.next().unwrap(), ',')
            .compile()
            .unwrap();
        let r = p.parse(lines.next().unwrap()).unwrap();
        assert_eq!(r.get(1), Some("x, \"y\""));
    }
}
