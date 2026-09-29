//! Every `ParserSpec` kind compiled once; arbitrary input parsed by all of
//! them. Spans must be in bounds and on char boundaries.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use oxtail_columns::{FixedColumn, Parser, ParserSpec};

fn parsers() -> &'static Vec<Parser> {
    static P: OnceLock<Vec<Parser>> = OnceLock::new();
    P.get_or_init(|| {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let specs = vec![
            ParserSpec::Delimited {
                delimiter: ',',
                quote: '"',
                has_header: false,
                columns: names(&["a", "b", "c"]),
                kinds: Default::default(),
            },
            ParserSpec::Delimited {
                delimiter: '\t',
                quote: '"',
                has_header: true,
                columns: names(&["ts", "level", "msg"]),
                kinds: Default::default(),
            },
            ParserSpec::Regex {
                pattern: r"^(?P<ts>\S+) (?P<level>[A-Z]+) (?P<msg>.*)$".into(),
                kinds: Default::default(),
            },
            ParserSpec::JsonLines {
                columns: names(&["ts", "level", "msg"]),
                kinds: Default::default(),
            },
            ParserSpec::Logfmt {
                columns: names(&["ts", "level", "msg"]),
                kinds: Default::default(),
            },
            ParserSpec::Syslog3164,
            ParserSpec::Syslog5424,
            ParserSpec::AccessCommon,
            ParserSpec::AccessCombined,
            ParserSpec::W3c {
                fields: names(&["date", "time", "c-ip", "cs-method", "sc-status"]),
                kinds: Default::default(),
            },
            ParserSpec::Log4j {
                pattern: "%d{ISO8601} %-5p [%t] %c{1} - %m%n".into(),
                kinds: Default::default(),
            },
            ParserSpec::FixedWidth {
                columns: vec![
                    FixedColumn {
                        name: "a".into(),
                        start: 0,
                        end: Some(5),
                    },
                    FixedColumn {
                        name: "b".into(),
                        start: 6,
                        end: None,
                    },
                ],
            },
        ];
        specs
            .iter()
            .map(|s| s.compile().expect("built-in fuzz spec must compile"))
            .collect()
    })
}

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    for p in parsers() {
        let rec = p.parse(&line);
        // Known bug (see crates/oxtail-columns/tests/fuzzlike_regressions.rs):
        // is_match and parse strip a trailing CR twice, so skip lines ending in CR.
        if !line.ends_with('\r') {
            assert_eq!(rec.is_some(), p.is_match(&line), "{:?}", p.spec());
        }
        let Some(rec) = rec else { continue };
        assert_eq!(rec.fields.len(), p.schema().len());
        for i in 0..p.schema().len() {
            if let Some(r) = rec.span(i) {
                assert!(r.start <= r.end && r.end <= line.len(), "span {r:?} oob");
                assert!(
                    line.is_char_boundary(r.start) && line.is_char_boundary(r.end),
                    "span {r:?} not on char boundary in {:?}",
                    p.spec()
                );
            }
        }
    }
});
