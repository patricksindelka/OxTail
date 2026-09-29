use super::*;
use crate::model::ColumnInfo;
use crate::timestamp::Rfc3339Fallback;

fn schema() -> Schema {
    Schema {
        columns: vec![
            ColumnInfo::new("ts", ColumnKind::Timestamp),
            ColumnInfo::new("level", ColumnKind::Level),
            ColumnInfo::new("status", ColumnKind::Number),
            ColumnInfo::new("duration", ColumnKind::Duration),
            ColumnInfo::new("size", ColumnKind::Bytes),
            ColumnInfo::new("path", ColumnKind::Text),
            ColumnInfo::new("msg", ColumnKind::Text),
        ],
    }
}

fn record(vals: [&'static str; 7]) -> Record<'static> {
    let mut r = Record::with_columns(7);
    for (i, v) in vals.iter().enumerate() {
        if !v.is_empty() {
            r.set_owned(i, v.to_string());
        }
    }
    r
}

const NOW: &str = "2026-09-29T12:00:00Z";

fn now_ns() -> i64 {
    Rfc3339Fallback.parse_ns(NOW).unwrap()
}

/// Evaluates `q` against a record (line = space-joined fields).
fn run(q: &str, vals: [&'static str; 7]) -> bool {
    let query = Query::parse_with_now(q, now_ns()).unwrap_or_else(|e| panic!("{q}: {e}"));
    let rec = record(vals);
    let line = vals.join(" ");
    query.matches(&line, Some(&rec), Some(&schema()), &Rfc3339Fallback)
}

const E: &str = "";

#[test]
fn table_of_queries() {
    // (query, [ts, level, status, duration, size, path, msg], expected)
    let cases: &[(&str, [&str; 7], bool)] = &[
        ("level:ERROR", [E, "error", E, E, E, E, E], true),
        ("level:ERROR", [E, "INFO", E, E, E, E, E], false),
        ("level:warning", [E, "W", E, E, E, E, E], true),
        ("level:(ERROR|FATAL)", [E, "Fatal", E, E, E, E, E], true),
        ("level:(ERROR | \"WARN\")", [E, "warn", E, E, E, E, E], true),
        ("level:ERROR|FATAL", [E, "fatal", E, E, E, E, E], true),
        ("level:(ERROR|FATAL)", [E, "info", E, E, E, E, E], false),
        ("level>=WARN", [E, "ERROR", E, E, E, E, E], true),
        ("level>=WARN", [E, "info", E, E, E, E, E], false),
        ("severity:error", [E, "error", E, E, E, E, E], true),
        ("status>=500", [E, E, "503", E, E, E, E], true),
        ("status>=500", [E, E, "200", E, E, E, E], false),
        ("status>=500", [E, E, E, E, E, E, E], false),
        ("status:200", [E, E, "200", E, E, E, E], true),
        ("status!=200", [E, E, "404", E, E, E, E], true),
        ("status!=200", [E, E, "200", E, E, E, E], false),
        ("status>199 status<300", [E, E, "204", E, E, E, E], true),
        ("duration>250ms", [E, E, E, "300ms", E, E, E], true),
        ("duration>250ms", [E, E, E, "1.2s", E, E, E], true),
        ("duration>250ms", [E, E, E, "200ms", E, E, E], false),
        ("duration>250ms", [E, E, E, "300", E, E, E], true),
        ("duration<=1s", [E, E, E, "1000ms", E, E, E], true),
        ("duration>1.5s", [E, E, E, "1500ms", E, E, E], false),
        ("duration>2µs", [E, E, E, "3us", E, E, E], true),
        ("size>1KB", [E, E, E, E, "1500", E, E], true),
        ("size>1KB", [E, E, E, E, "1KiB", E, E], true),
        ("size>1KiB", [E, E, E, E, "1000", E, E], false),
        ("size<=2MB", [E, E, E, E, "1500000", E, E], true),
        (
            "msg~\"timeout after \\d+\"",
            [E, E, E, E, E, E, "timeout after 30s"],
            true,
        ),
        (
            "msg~\"timeout after \\d+\"",
            [E, E, E, E, E, E, "timeout after x"],
            false,
        ),
        ("msg~(foo|bar)+", [E, E, E, E, E, E, "xbarfoo"], true),
        ("msg!~foo", [E, E, E, E, E, E, "bar"], true),
        ("-path:/health", [E, E, E, E, E, "/health", E], false),
        ("-path:/health", [E, E, E, E, E, "/api", E], true),
        ("NOT path:/health", [E, E, E, E, E, "/HEALTH", E], false),
        ("path:/api/*", [E, E, E, E, E, "/api/users/1", E], true),
        ("path:*.css", [E, E, E, E, E, "/a/b.CSS", E], true),
        ("path:/api/*", [E, E, E, E, E, "/other", E], false),
        ("path:\"/a b\"", [E, E, E, E, E, "/a b", E], true),
        ("timeout", [E, E, E, E, E, E, "Request TIMEOUT hit"], true),
        (
            "\"connection reset\"",
            [E, E, E, E, E, E, "Connection Reset by peer"],
            true,
        ),
        ("timeout", [E, E, E, E, E, E, "fine"], false),
        (
            "http://example",
            [E, E, E, E, E, E, "GET http://example.com"],
            true,
        ),
        (
            "level:ERROR timeout",
            [E, "error", E, E, E, E, "a timeout"],
            true,
        ),
        (
            "level:ERROR timeout",
            [E, "info", E, E, E, E, "a timeout"],
            false,
        ),
        (
            "level:ERROR OR level:FATAL",
            [E, "fatal", E, E, E, E, E],
            true,
        ),
        (
            "level:ERROR AND status>=500",
            [E, "error", "200", E, E, E, E],
            false,
        ),
        (
            "(level:ERROR OR level:WARN) status>=500",
            [E, "warn", "500", E, E, E, E],
            true,
        ),
        (
            "level:ERROR OR level:WARN status>=500",
            [E, "error", "200", E, E, E, E],
            true,
        ),
        ("NOT NOT level:info", [E, "info", E, E, E, E, E], true),
        ("--level:info", [E, "info", E, E, E, E, E], true),
        ("", [E, "info", E, E, E, E, E], true),
        ("nosuchcol:x", [E, "info", E, E, E, E, E], false),
        // time
        (
            "ts>2026-09-29T10:00",
            ["2026-09-29T10:05:00Z", E, E, E, E, E, E],
            true,
        ),
        (
            "ts>2026-09-29T10:00",
            ["2026-09-29T09:59:00Z", E, E, E, E, E, E],
            false,
        ),
        (
            "ts>2026-09-29T10:00 ts<+15m",
            ["2026-09-29T10:10:00Z", E, E, E, E, E, E],
            true,
        ),
        (
            "ts>2026-09-29T10:00 ts<+15m",
            ["2026-09-29T10:20:00Z", E, E, E, E, E, E],
            false,
        ),
        ("ts>-1h", ["2026-09-29T11:30:00Z", E, E, E, E, E, E], true),
        ("ts>-1h", ["2026-09-29T10:30:00Z", E, E, E, E, E, E], false),
        (
            "ts>-1h ts<-30m",
            ["2026-09-29T11:15:00Z", E, E, E, E, E, E],
            true,
        ),
        (
            "ts<\"2026-09-29 10:00:00\"",
            ["2026-09-29T09:00:00Z", E, E, E, E, E, E],
            true,
        ),
        (
            "ts:2026-09-29",
            ["2026-09-29T09:00:00Z", E, E, E, E, E, E],
            true,
        ),
        (
            "time>2026-09-29T10:00",
            ["2026-09-29T10:05:00Z", E, E, E, E, E, E],
            true,
        ),
    ];
    for (q, vals, expect) in cases {
        assert_eq!(run(q, *vals), *expect, "query {q:?} on {vals:?}");
    }
}

#[test]
fn extra_keys_are_queryable() {
    let q = Query::parse("http.status>=500 -user:bob").unwrap();
    let mut r = Record::with_columns(1);
    r.extra.push(("http.status".into(), "503".into()));
    r.extra.push(("user".into(), "alice".into()));
    let s = Schema {
        columns: vec![ColumnInfo::new("msg", ColumnKind::Text)],
    };
    assert!(q.matches("x", Some(&r), Some(&s), &Rfc3339Fallback));
    r.extra[1].1 = "BOB".into();
    assert!(!q.matches("x", Some(&r), Some(&s), &Rfc3339Fallback));
}

#[test]
fn plain_text_without_records() {
    let q = Query::parse("error -debug").unwrap();
    assert!(q.matches("an ERROR occurred", None, None, &Rfc3339Fallback));
    assert!(!q.matches("an ERROR debug occurred", None, None, &Rfc3339Fallback));
    let c = Query::parse("level:error").unwrap();
    assert!(!c.matches("level error", None, None, &Rfc3339Fallback));
    assert!(Query::parse("").unwrap().is_empty());
}

#[test]
fn non_ascii_text() {
    let q = Query::parse("straße").unwrap();
    assert!(q.matches("Hauptstraße 5", None, None, &Rfc3339Fallback));
    let q = Query::parse("ÉCOLE").unwrap();
    assert!(q.matches("une école", None, None, &Rfc3339Fallback));
}

#[test]
fn error_positions() {
    let cases: &[(&str, usize, &str)] = &[
        ("level:", 6, "expected a value after 'level:'"),
        ("status>= 5", 8, "expected a value after 'status>='"),
        ("\"abc", 0, "unterminated quote"),
        ("a \"abc", 2, "unterminated quote"),
        ("(a b", 0, "missing ')'"),
        ("a)", 1, "unmatched ')'"),
        ("a AND", 2, "after 'AND'"),
        ("a OR", 4, "end of the query"),
        ("OR a", 0, "before 'OR'"),
        ("AND a", 0, "before 'AND'"),
        ("()", 1, "before ')'"),
        ("NOT ", 0, "after 'NOT'"),
        ("msg~\"(\"", 4, "invalid regular expression"),
        ("status>(1|2)", 7, "only be used with ':'"),
        ("x:a||b", 4, "empty alternative"),
        ("x:(a b)", 5, "expected '|' or ')'"),
        ("x:(a|", 5, "expected a value"),
        ("x:(a", 2, "missing ')'"),
        ("x:()", 3, "expected a value"),
    ];
    for (q, pos, needle) in cases {
        let e = Query::parse(q).unwrap_err();
        assert_eq!(e.position, *pos, "{q:?}: {e}");
        assert!(e.message.contains(needle), "{q:?}: {e}");
    }
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let q = "(".repeat(10_000);
    assert!(Query::parse(&q).is_err());
    let q = format!("{}a{}", "(".repeat(64), ")".repeat(64));
    assert!(Query::parse(&q).is_ok());
    let q = "NOT ".repeat(100_000) + "a";
    assert!(Query::parse(&q).is_ok());
}

#[test]
fn check_reports_unknown_columns() {
    let q = Query::parse("lvl:ERROR foo:1 bar>2 -msg:x").unwrap();
    let errs = q.check(&schema());
    let names: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(errs.len(), 2, "{names:?}");
    assert_eq!(errs[0].position, 10);
    assert!(errs[0].message.contains("unknown column 'foo'"));
    assert!(errs[0].message.contains("available: ts, level"));
    assert!(Query::parse("level:x").unwrap().check(&schema()).is_empty());
    let e = Query::parse("ts<+15m").unwrap().check(&schema());
    assert_eq!(e.len(), 1);
    assert!(e[0].message.contains("lower bound"));
    assert!(
        Query::parse("ts>2026-01-01 ts<+15m")
            .unwrap()
            .check(&schema())
            .is_empty()
    );
}

mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parse_and_match_never_panic(q in any::<String>(), line in any::<String>(), cell in any::<String>()) {
            if let Ok(query) = Query::parse(&q) {
                let mut r = Record::with_columns(7);
                for i in 0..7 { r.set_owned(i, cell.clone()); }
                let _ = query.matches(&line, Some(&r), Some(&schema()), &Rfc3339Fallback);
                let _ = query.matches(&line, None, None, &Rfc3339Fallback);
                let _ = query.check(&schema());
            }
        }

        #[test]
        fn token_soup_never_panics(
            toks in proptest::collection::vec(
                prop_oneof![
                    Just("level".to_string()), Just(":".to_string()), Just(">=".to_string()),
                    Just("(".to_string()), Just(")".to_string()), Just("|".to_string()),
                    Just("\"".to_string()), Just("-".to_string()), Just("OR".to_string()),
                    Just("AND".to_string()), Just("NOT".to_string()), Just("~".to_string()),
                    Just("250ms".to_string()), Just("+15m".to_string()), Just("ts".to_string()),
                    Just(" ".to_string()), Just("!=".to_string()), Just("*".to_string()),
                    "[a-z0-9é]{1,4}",
                ], 0..14),
            cell in "[ -~]{0,12}"
        ) {
            let q: String = toks.concat();
            if let Ok(query) = Query::parse(&q) {
                let mut r = Record::with_columns(7);
                for i in 0..7 { r.set_owned(i, cell.clone()); }
                let _ = query.matches("some line", Some(&r), Some(&schema()), &Rfc3339Fallback);
            }
        }

        #[test]
        fn text_term_agrees_with_naive(word in "[a-zA-Z]{1,6}", line in "[a-zA-Z ]{0,30}") {
            let q = Query::parse(&format!("\"{word}\"")).unwrap();
            let expect = line.to_lowercase().contains(&word.to_lowercase());
            prop_assert_eq!(q.matches(&line, None, None, &Rfc3339Fallback), expect);
        }
    }
}
