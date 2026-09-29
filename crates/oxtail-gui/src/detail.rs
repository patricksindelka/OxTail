//! The detail pane: the selected record as key/value pairs, JSON cells
//! pretty-printed, and "copy as JSON / CSV". Pure functions over an
//! `oxtail_columns::Record`, so they are unit-tested without a UI.

use oxtail_columns::{ColumnKind, Record, Schema, export_csv, export_jsonl};
use serde_json::{Map, Value};

/// One row of the detail list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailField {
    /// Column or key name.
    pub name: String,
    /// The value as shown: JSON cells pretty-printed, everything else raw.
    pub value: String,
    /// The column kind (`Text` for extra keys).
    pub kind: ColumnKind,
    /// A key that is not in the schema (JSON / logfmt extras).
    pub extra: bool,
    /// The value is multi-line (pretty-printed JSON).
    pub multiline: bool,
}

/// Pretty-prints `value` when it is a JSON object or array (regardless of the
/// column kind: extra keys of JSON lines are often nested), else returns it
/// unchanged.
pub fn pretty_value(kind: ColumnKind, value: &str) -> String {
    let t = value.trim_start();
    let looks_json = kind == ColumnKind::Json || t.starts_with('{') || t.starts_with('[');
    if looks_json
        && let Ok(v) = serde_json::from_str::<Value>(value)
        && (v.is_object() || v.is_array())
        && let Ok(s) = serde_json::to_string_pretty(&v)
    {
        return s;
    }
    value.to_string()
}

/// The fields of a record: every present schema column, then the extra keys.
pub fn detail_fields(schema: &Schema, rec: &Record<'_>) -> Vec<DetailField> {
    let mut out = Vec::new();
    for (i, c) in schema.columns.iter().enumerate() {
        let Some(v) = rec.get(i) else { continue };
        let value = pretty_value(c.kind, v);
        out.push(DetailField {
            name: c.name.clone(),
            multiline: value.contains('\n'),
            value,
            kind: c.kind,
            extra: false,
        });
    }
    for (k, v) in &rec.extra {
        let value = pretty_value(ColumnKind::Text, v);
        out.push(DetailField {
            name: k.to_string(),
            multiline: value.contains('\n'),
            value,
            kind: ColumnKind::Text,
            extra: true,
        });
    }
    out
}

/// The record as one pretty-printed JSON object: schema columns (typed like
/// the JSON Lines export) followed by the extra keys.
pub fn record_json(schema: &Schema, rec: &Record<'_>) -> String {
    let cols: Vec<usize> = (0..schema.len()).collect();
    let mut buf = Vec::new();
    // Writing to a Vec cannot fail; fall back to an empty object anyway.
    let mut obj = export_jsonl(&mut buf, schema, &cols, [rec])
        .ok()
        .and_then(|_| serde_json::from_slice::<Map<String, Value>>(&buf).ok())
        .unwrap_or_default();
    for (k, v) in &rec.extra {
        let value =
            serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        obj.insert(k.to_string(), value);
    }
    serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// The record as CSV: a header row and one data row.
pub fn record_csv(schema: &Schema, rec: &Record<'_>) -> String {
    let cols: Vec<usize> = (0..schema.len()).collect();
    let mut buf = Vec::new();
    let _ = export_csv(&mut buf, schema, &cols, [rec]);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtail_columns::ParserSpec;

    fn json_parser() -> oxtail_columns::Parser {
        ParserSpec::JsonLines {
            columns: vec!["ts".into(), "level".into(), "http.status".into()],
            kinds: Default::default(),
        }
        .compile()
        .unwrap()
    }

    const LINE: &str = r#"{"ts":"2026-09-29T10:00:00Z","level":"error","http":{"status":502},"msg":"upstream down","ctx":{"a":1,"b":[1,2]}}"#;

    #[test]
    fn fields_include_extra_keys_and_pretty_print_nested_json() {
        let p = json_parser();
        let rec = p.parse(LINE).unwrap();
        let f = detail_fields(p.schema(), &rec);
        let names: Vec<&str> = f.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(&names[..3], ["ts", "level", "http.status"]);
        // Nested objects are flattened by the parser; arrays stay JSON text.
        let a = f.iter().find(|x| x.name == "ctx.a").expect("extra key");
        assert!(a.extra && !a.multiline && a.value == "1");
        let b = f.iter().find(|x| x.name == "ctx.b").expect("extra key");
        assert!(b.extra && b.multiline, "{b:?}");
        assert!(b.value.contains("  2"));
        let msg = f.iter().find(|x| x.name == "msg").unwrap();
        assert!(msg.extra && !msg.multiline);
        assert_eq!(msg.value, "upstream down");
    }

    #[test]
    fn absent_columns_are_left_out() {
        let p = json_parser();
        let rec = p.parse(r#"{"level":"info"}"#).unwrap();
        let f = detail_fields(p.schema(), &rec);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "level");
    }

    #[test]
    fn json_export_keeps_types_and_extras() {
        let p = json_parser();
        let rec = p.parse(LINE).unwrap();
        let v: Value = serde_json::from_str(&record_json(p.schema(), &rec)).unwrap();
        assert_eq!(v["level"], "error");
        assert_eq!(v["http.status"], 502);
        assert_eq!(v["msg"], "upstream down");
        assert_eq!(v["ctx.a"], 1);
        assert_eq!(v["ctx.b"][1], 2);
    }

    #[test]
    fn csv_export_has_header_and_quotes() {
        let p = ParserSpec::Logfmt {
            columns: vec!["a".into(), "b".into()],
            kinds: Default::default(),
        }
        .compile()
        .unwrap();
        let rec = p.parse("a=1 b=\"x, \\\"y\\\"\"").unwrap();
        let csv = record_csv(p.schema(), &rec);
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("a,b"));
        assert_eq!(lines.next(), Some("1,\"x, \"\"y\"\"\""));
    }

    #[test]
    fn pretty_value_only_touches_json() {
        assert_eq!(pretty_value(ColumnKind::Text, "plain"), "plain");
        assert_eq!(pretty_value(ColumnKind::Text, "{not json"), "{not json");
        assert_eq!(pretty_value(ColumnKind::Json, "42"), "42");
        assert!(pretty_value(ColumnKind::Json, "{\"a\":1}").contains('\n'));
        assert!(pretty_value(ColumnKind::Text, "[1,2]").contains('\n'));
    }
}
