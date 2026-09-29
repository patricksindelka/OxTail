//! Parser auto-detection by coverage and consistency over sampled lines.
//!
//! For every candidate format the sample is parsed; the score is
//! `coverage * (0.6 + 0.4 * consistency) * specificity`, where
//!
//! * *coverage* is the fraction of lines that parse, ignoring blank lines,
//!   directive lines (`#...`) and continuation-looking lines (stack traces);
//! * *consistency* is the share of parsed records that fill the same set of
//!   columns (the modal shape);
//! * *specificity* prefers exact built-in formats over generic ones.
//!
//! Candidates scoring below 0.5 are dropped, so plain prose yields nothing.

use std::collections::HashMap;
use std::collections::HashSet;

use regex::Regex;

use crate::group::RecordGrouper;
use crate::parsers::delimited::Delimited;
use crate::parsers::{Parser, ParserSpec, discover_json_columns, discover_logfmt_columns, logfmt};

/// A candidate parser for a sample.
#[derive(Debug, Clone)]
pub struct Detection {
    /// The parser definition, ready to compile.
    pub spec: ParserSpec,
    /// Human-readable format name, e.g. `"Nginx/Apache combined"`.
    pub name: String,
    /// Score in `0.0..=1.0`; higher is better.
    pub score: f32,
}

const MIN_SCORE: f32 = 0.5;
/// Only this many lines of the sample are examined.
const MAX_SAMPLE: usize = 2000;

/// Detects the best parsers for `sample`, best first. Returns an empty list
/// for plain prose or when nothing reaches the minimum score.
pub fn detect(sample: &[&str]) -> Vec<Detection> {
    let lines: Vec<&str> = sample
        .iter()
        .take(MAX_SAMPLE)
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Detection> = Vec::new();
    let mut add = |spec: ParserSpec, name: String, score: Option<f32>| {
        if let Some(score) = score.filter(|s| *s >= MIN_SCORE) {
            out.push(Detection { spec, name, score });
        }
    };

    // Lines that count towards coverage: not directives, not continuations.
    let considered: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.starts_with('#') && !RecordGrouper::looks_like_continuation(l))
        .collect();

    // Built-in fixed formats.
    for spec in [
        ParserSpec::AccessCombined,
        ParserSpec::AccessCommon,
        ParserSpec::Syslog5424,
        ParserSpec::Syslog3164,
    ] {
        let name = spec.display_name().to_string();
        let s = score(&spec, &considered, 1.0, |_, _| true);
        add(spec, name, s);
    }

    // W3C / IIS: needs a #Fields directive in the sample.
    if let Some(spec) = lines
        .iter()
        .find_map(|l| ParserSpec::w3c_from_fields_line(l))
    {
        let name = spec.display_name().to_string();
        let s = score(&spec, &considered, 1.0, |_, _| true);
        add(spec, name, s);
    }

    // JSON Lines.
    if considered.iter().any(|l| l.trim_start().starts_with('{')) {
        let cols = discover_json_columns(&considered);
        let spec = ParserSpec::JsonLines {
            columns: cols,
            kinds: Default::default(),
        };
        let name = spec.display_name().to_string();
        let s = score(&spec, &considered, 1.0, |_, _| true);
        add(spec, name, s);
    }

    // logfmt: at least two pairs and mostly key=value tokens.
    {
        let cols = discover_logfmt_columns(&considered);
        let spec = ParserSpec::Logfmt {
            columns: cols,
            kinds: Default::default(),
        };
        let name = spec.display_name().to_string();
        let s = score(&spec, &considered, 0.97, |line, _| logfmt_like(line));
        add(spec, name, s);
    }

    // Delimited.
    if let Some((spec, name, s)) = detect_delimited(&lines) {
        add(spec, name, Some(s));
    }

    // "timestamp level message" text logs.
    for (spec, name, factor) in text_log_candidates(&considered) {
        let s = score(&spec, &considered, factor, |_, _| true);
        add(spec, name, s);
    }

    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Scores `spec` over `lines`; `None` if it does not compile or nothing matches.
fn score(
    spec: &ParserSpec,
    lines: &[&str],
    specificity: f32,
    accept: impl Fn(&str, &crate::model::Record<'_>) -> bool,
) -> Option<f32> {
    let parser = spec.compile().ok()?;
    score_parser(&parser, lines, specificity, accept)
}

fn score_parser(
    parser: &Parser,
    lines: &[&str],
    specificity: f32,
    accept: impl Fn(&str, &crate::model::Record<'_>) -> bool,
) -> Option<f32> {
    if lines.is_empty() {
        return None;
    }
    let mut matched = 0usize;
    let mut shapes: HashMap<Vec<bool>, usize> = HashMap::new();
    for line in lines {
        if let Some(rec) = parser.parse(line) {
            if accept(line, &rec) {
                matched += 1;
                let shape: Vec<bool> = rec.fields.iter().map(Option::is_some).collect();
                *shapes.entry(shape).or_insert(0) += 1;
            }
        }
    }
    if matched == 0 {
        return None;
    }
    let coverage = matched as f32 / lines.len() as f32;
    let modal = shapes.values().copied().max().unwrap_or(0);
    let consistency = modal as f32 / matched as f32;
    Some((coverage * (0.6 + 0.4 * consistency) * specificity).clamp(0.0, 1.0))
}

/// True if most whitespace-separated tokens are `key=value` and there are at
/// least two of them.
fn logfmt_like(line: &str) -> bool {
    let mut tokens = 0usize;
    let pairs = logfmt::scan(line, |_, _, _| tokens += 1);
    pairs >= 2 && pairs * 2 >= tokens
}

fn looks_numeric(s: &str) -> bool {
    s.trim().parse::<f64>().is_ok()
}

fn detect_delimited(lines: &[&str]) -> Option<(ParserSpec, String, f32)> {
    let mut best: Option<(ParserSpec, String, f32)> = None;
    for delim in [',', '\t', '|', ';'] {
        let d = Delimited::new(delim, '"', Vec::new(), false);
        let counts: Vec<usize> = lines
            .iter()
            .filter(|l| !l.starts_with('#'))
            .map(|l| d.split(l, 0).1)
            .collect();
        if counts.len() < 2 && counts.first().is_none_or(|&c| c < 3) {
            continue;
        }
        let mut freq: HashMap<usize, usize> = HashMap::new();
        for &c in &counts {
            *freq.entry(c).or_insert(0) += 1;
        }
        let Some((&modal, &n)) = freq.iter().max_by_key(|(c, n)| (**n, **c)) else {
            continue;
        };
        if modal < 2 || (n as f32) / (counts.len() as f32) < 0.8 {
            continue;
        }
        // Header row?
        let first = d.split_owned(lines[0]);
        let header = first.len() == modal
            && first
                .iter()
                .all(|c| !c.trim().is_empty() && c.len() <= 40 && !looks_numeric(c))
            && first.iter().collect::<HashSet<_>>().len() == first.len()
            && lines.len() > 1
            && lines[1..].iter().take(20).any(|l| {
                d.split_owned(l)
                    .iter()
                    .zip(&first)
                    .any(|(v, _)| looks_numeric(v))
            });
        let spec = if header {
            ParserSpec::delimited_from_header_line(lines[0], delim)
        } else {
            ParserSpec::Delimited {
                delimiter: delim,
                quote: '"',
                has_header: false,
                columns: (1..=modal).map(|i| format!("col{i}")).collect(),
                kinds: Default::default(),
            }
        };
        let name = format!("{} ({modal} columns)", spec.display_name());
        let considered: Vec<&str> = if header {
            lines[1..].to_vec()
        } else {
            lines.to_vec()
        };
        let considered: Vec<&str> = considered
            .into_iter()
            .filter(|l| !l.starts_with('#'))
            .collect();
        let Some(s) = score(&spec, &considered, 0.85, |_, _| true) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| s > b.2) {
            best = Some((spec, name, s));
        }
    }
    best
}

const TS_FORMATS: &[&str] = &[
    r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
    r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}(?:[.,]\d+)?",
    r"\d{2}/\d{2}/\d{4} \d{2}:\d{2}:\d{2}(?:[.,]\d+)?",
    r"[A-Z][a-z]{2} [ \d]\d \d{2}:\d{2}:\d{2}",
    r"\d{2}:\d{2}:\d{2}(?:[.,]\d+)?",
    r"\d{10,13}(?:\.\d+)?",
];

const LEVELS: &str =
    r"(?i:trace|debug|info|notice|warn(?:ing)?|error|err|fatal|crit(?:ical)?|severe)";

/// Regex candidates for "timestamp level message"-shaped text logs. Only
/// timestamp formats that start at least half of the sample are tried.
fn text_log_candidates(lines: &[&str]) -> Vec<(ParserSpec, String, f32)> {
    let mut out = Vec::new();
    let half = lines.len().div_ceil(2);
    for ts in TS_FORMATS {
        let Ok(prefix) = Regex::new(&format!(r"^\[?{ts}")) else {
            continue;
        };
        let hits = lines.iter().filter(|l| prefix.is_match(l)).count();
        let level_first = Regex::new(&format!(r"^\[?{LEVELS}\]?\s+\[?{ts}"))
            .map(|r| lines.iter().filter(|l| r.is_match(l)).count())
            .unwrap_or(0);
        if hits < half && level_first < half {
            continue;
        }
        let sep = r"\s+(?:[-|:]\s+)?";
        let mk = |pattern: String, name: &str, factor: f32| {
            (
                ParserSpec::Regex {
                    pattern,
                    kinds: Default::default(),
                },
                name.to_string(),
                factor,
            )
        };
        out.push(mk(
            format!(r"^\[?(?P<ts>{ts})\]?{sep}\[?(?P<level>{LEVELS})\]?\b:?\s+(?P<msg>.*)$"),
            "Timestamp, level, message",
            0.95,
        ));
        out.push(mk(
            format!(
                r"^\[?(?P<ts>{ts})\]?{sep}\[?(?P<level>{LEVELS})\]?\s+\[(?P<thread>[^\]]*)\]\s+(?P<msg>.*)$"
            ),
            "Timestamp, level, [thread], message",
            0.96,
        ));
        out.push(mk(
            format!(
                r"^\[?(?P<ts>{ts})\]?{sep}\[?(?P<level>{LEVELS})\]?\s+\[(?P<thread>[^\]]*)\]\s+(?P<logger>[^\s\[\]:]+)\s+[-:]\s+(?P<msg>.*)$"
            ),
            "Log4j style (timestamp level [thread] logger - message)",
            0.97,
        ));
        out.push(mk(
            format!(r"^\[?(?P<level>{LEVELS})\]?\s+\[?(?P<ts>{ts})\]?\s+(?P<msg>.*)$"),
            "Level, timestamp, message",
            0.9,
        ));
        out.push(mk(
            format!(r"^\[?(?P<ts>{ts})\]?\s+(?P<msg>.*)$"),
            "Timestamp, message",
            0.6,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(lines: &[&str]) -> Option<Detection> {
        detect(lines).into_iter().next()
    }

    fn name(lines: &[&str]) -> String {
        top(lines).map(|d| d.name).unwrap_or_default()
    }

    #[test]
    fn nginx_combined_and_apache_common() {
        let combined = [
            r#"93.184.216.34 - - [29/Sep/2026:10:00:01 +0000] "GET / HTTP/1.1" 200 1532 "-" "curl/8.0""#,
            r#"10.0.0.2 - bob [29/Sep/2026:10:00:02 +0000] "POST /login HTTP/1.1" 302 0 "https://x/" "Mozilla/5.0""#,
        ];
        let d = top(&combined).unwrap();
        assert!(matches!(d.spec, ParserSpec::AccessCombined), "{d:?}");
        let common = [
            r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /a.gif HTTP/1.0" 200 2326"#,
            r#"127.0.0.1 - - [10/Oct/2000:13:55:37 -0700] "GET /b.gif HTTP/1.0" 404 -"#,
        ];
        let d = top(&common).unwrap();
        assert!(matches!(d.spec, ParserSpec::AccessCommon), "{d:?}");
    }

    #[test]
    fn syslog_variants() {
        let s3164 = [
            "<34>Oct 11 22:14:15 mymachine su[1]: 'su root' failed",
            "<13>Oct 11 22:14:16 mymachine cron: job started",
        ];
        assert!(matches!(top(&s3164).unwrap().spec, ParserSpec::Syslog3164));
        let s5424 = [
            "<165>1 2003-10-11T22:14:15.003Z host evntslog - ID47 - An event",
            "<165>1 2003-10-11T22:14:16.003Z host evntslog - ID48 [a@1 b=\"c\"] Another",
        ];
        assert!(matches!(top(&s5424).unwrap().spec, ParserSpec::Syslog5424));
    }

    #[test]
    fn jsonl_and_logfmt() {
        let j = [
            r#"{"ts":"2026-09-29T10:00:00Z","level":"info","msg":"a"}"#,
            r#"{"ts":"2026-09-29T10:00:01Z","level":"warn","msg":"b","extra":1}"#,
        ];
        let d = top(&j).unwrap();
        assert!(matches!(d.spec, ParserSpec::JsonLines { .. }), "{d:?}");
        let l = [
            r#"ts=2026-09-29T10:00:00Z level=info msg="hello world" dur=12ms"#,
            r#"ts=2026-09-29T10:00:01Z level=warn msg="slow" dur=1200ms"#,
        ];
        let d = top(&l).unwrap();
        assert!(matches!(d.spec, ParserSpec::Logfmt { .. }), "{d:?}");
    }

    #[test]
    fn csv_tsv_with_header() {
        let csv = [
            "id,name,score",
            "1,alice,9.5",
            "2,\"Smith, Bob\",7",
            "3,carol,8",
        ];
        let d = top(&csv).unwrap();
        match &d.spec {
            ParserSpec::Delimited {
                delimiter,
                has_header,
                columns,
                ..
            } => {
                assert_eq!(*delimiter, ',');
                assert!(*has_header);
                assert_eq!(columns, &["id", "name", "score"]);
            }
            other => panic!("{other:?}"),
        }
        let tsv = ["a\tb\tc", "1\tx\t2", "3\ty\t4"];
        let d = top(&tsv).unwrap();
        assert!(matches!(
            d.spec,
            ParserSpec::Delimited {
                delimiter: '\t',
                ..
            }
        ));
    }

    #[test]
    fn w3c_needs_fields_directive() {
        let w = [
            "#Software: Microsoft Internet Information Services 10.0",
            "#Version: 1.0",
            "#Fields: date time s-ip cs-method cs-uri-stem sc-status",
            "2026-09-29 10:00:01 10.0.0.5 GET /index.html 200",
            "2026-09-29 10:00:02 10.0.0.5 GET /a.css 304",
        ];
        let d = top(&w).unwrap();
        assert!(matches!(d.spec, ParserSpec::W3c { .. }), "{d:?}");
    }

    #[test]
    fn timestamp_level_message_text() {
        let t = [
            "2026-09-29 10:00:00,123 INFO Starting server on port 8080",
            "2026-09-29 10:00:01,456 WARN Disk usage at 91%",
            "\tat com.example.Foo.bar(Foo.java:12)",
            "2026-09-29 10:00:02,789 ERROR Failed to connect: timeout",
        ];
        let d = top(&t).unwrap();
        match &d.spec {
            ParserSpec::Regex { pattern, .. } => {
                assert!(pattern.contains("(?P<ts>") && pattern.contains("(?P<level>"));
            }
            other => panic!("{other:?}"),
        }
        assert!(d.score > 0.85, "{d:?}");
        let p = d.spec.compile().unwrap();
        assert!(p.parse(t[0]).is_some());
        let l4j = [
            "2026-09-29 10:00:01,123 INFO  [main] c.e.App - started",
            "2026-09-29 10:00:02,123 ERROR [worker-1] c.e.Db - failed",
        ];
        assert!(name(&l4j).starts_with("Log4j style"), "{}", name(&l4j));
    }

    #[test]
    fn prose_yields_nothing() {
        let prose = [
            "The quick brown fox jumps over the lazy dog.",
            "Well, it was a dark and stormy night; the rain fell in torrents.",
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod.",
            "",
            "Short line",
        ];
        assert!(detect(&prose).is_empty(), "{:?}", detect(&prose));
        assert!(detect(&[]).is_empty());
        assert!(detect(&["Hello, world"]).is_empty());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn detect_never_panics(lines in proptest::collection::vec(any::<String>(), 0..12)) {
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                for d in detect(&refs) {
                    prop_assert!((0.0..=1.0).contains(&d.score));
                    // Every detection must compile.
                    prop_assert!(d.spec.compile().is_ok());
                }
            }

            #[test]
            fn detect_structured_lines_never_panics(
                lines in proptest::collection::vec("[ -~{}\\[\\]\",=:|;\\t]{0,60}", 0..12)
            ) {
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                let _ = detect(&refs);
            }
        }
    }
}
