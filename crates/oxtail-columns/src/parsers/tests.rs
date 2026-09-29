use super::*;

fn compile(spec: ParserSpec) -> Parser {
    spec.compile().expect("spec compiles")
}

fn get<'a>(p: &Parser, r: &'a Record<'_>, name: &str) -> Option<&'a str> {
    r.get(p.schema().find(name)?)
}

fn check_spans(line: &str, r: &Record<'_>) {
    for (i, s) in r.spans.iter().enumerate() {
        if let Some(s) = s {
            assert!(line.is_char_boundary(s.start) && line.is_char_boundary(s.end));
            assert_eq!(Some(&line[s.clone()]), r.get(i));
        }
    }
}

#[test]
fn nginx_combined() {
    let p = compile(ParserSpec::AccessCombined);
    let line = r#"93.184.216.34 - alice [29/Sep/2026:10:00:01 +0000] "GET /api/users?id=5 HTTP/1.1" 200 1532 "https://example.com/" "Mozilla/5.0 (X11; Linux x86_64)""#;
    let r = p.parse(line).expect("parses");
    assert_eq!(get(&p, &r, "remote"), Some("93.184.216.34"));
    assert_eq!(get(&p, &r, "user"), Some("alice"));
    assert_eq!(get(&p, &r, "ts"), Some("29/Sep/2026:10:00:01 +0000"));
    assert_eq!(get(&p, &r, "method"), Some("GET"));
    assert_eq!(get(&p, &r, "path"), Some("/api/users?id=5"));
    assert_eq!(get(&p, &r, "proto"), Some("HTTP/1.1"));
    assert_eq!(get(&p, &r, "status"), Some("200"));
    assert_eq!(get(&p, &r, "size"), Some("1532"));
    assert_eq!(get(&p, &r, "referer"), Some("https://example.com/"));
    assert_eq!(
        get(&p, &r, "agent"),
        Some("Mozilla/5.0 (X11; Linux x86_64)")
    );
    check_spans(line, &r);
    // Combined lines are not "common" lines.
    assert!(compile(ParserSpec::AccessCommon).parse(line).is_none());
}

#[test]
fn apache_common_and_garbage_request() {
    let p = compile(ParserSpec::AccessCommon);
    let line =
        r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326"#;
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "status"), Some("200"));
    assert_eq!(get(&p, &r, "size"), Some("2326"));
    let bad = r#"10.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "\x16\x03\x01" 400 -"#;
    let r = p.parse(bad).unwrap();
    assert_eq!(get(&p, &r, "method"), None);
    assert_eq!(get(&p, &r, "size"), Some("-"));
}

#[test]
fn syslog_3164() {
    let p = compile(ParserSpec::Syslog3164);
    let line = "<34>Oct 11 22:14:15 mymachine su[1234]: 'su root' failed for lonvick on /dev/pts/8";
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "pri"), Some("34"));
    assert_eq!(get(&p, &r, "ts"), Some("Oct 11 22:14:15"));
    assert_eq!(get(&p, &r, "host"), Some("mymachine"));
    assert_eq!(get(&p, &r, "tag"), Some("su"));
    assert_eq!(get(&p, &r, "pid"), Some("1234"));
    assert_eq!(
        get(&p, &r, "msg"),
        Some("'su root' failed for lonvick on /dev/pts/8")
    );
    // Without PRI, as found in /var/log/syslog
    let r = p
        .parse("Sep  3 06:25:01 web1 CRON[9911]: (root) CMD (run-parts)")
        .unwrap();
    assert_eq!(get(&p, &r, "ts"), Some("Sep  3 06:25:01"));
    assert_eq!(get(&p, &r, "pri"), None);
    check_spans(line, &p.parse(line).unwrap());
}

#[test]
fn syslog_5424() {
    let p = compile(ParserSpec::Syslog5424);
    let line = r#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventSource="Application" eventID="1011"] An application event log entry"#;
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "pri"), Some("165"));
    assert_eq!(get(&p, &r, "version"), Some("1"));
    assert_eq!(get(&p, &r, "ts"), Some("2003-10-11T22:14:15.003Z"));
    assert_eq!(get(&p, &r, "app"), Some("evntslog"));
    assert_eq!(get(&p, &r, "procid"), Some("-"));
    assert_eq!(get(&p, &r, "msgid"), Some("ID47"));
    assert!(get(&p, &r, "sd").unwrap().starts_with("[exampleSDID@32473"));
    assert_eq!(get(&p, &r, "msg"), Some("An application event log entry"));
    let r = p
        .parse(
            "<34>1 2003-10-11T22:14:15.003Z mymachine.example.com su - ID47 - BOM'su root' failed",
        )
        .unwrap();
    assert_eq!(get(&p, &r, "sd"), Some("-"));
    assert!(p.parse("<34>Oct 11 22:14:15 mymachine su: x").is_none());
}

#[test]
fn w3c_iis() {
    let spec = ParserSpec::w3c_from_fields_line(
        "#Fields: date time s-ip cs-method cs-uri-stem sc-status time-taken",
    )
    .unwrap();
    let p = compile(spec);
    assert_eq!(p.schema().len(), 7);
    assert!(
        p.parse("#Software: Microsoft Internet Information Services 10.0")
            .is_none()
    );
    assert!(p.parse("#Fields: date time").is_none());
    let line = "2026-09-29 10:00:01 10.0.0.5 GET /index.html 200 15";
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "cs-method"), Some("GET"));
    assert_eq!(get(&p, &r, "sc-status"), Some("200"));
    assert_eq!(p.schema().columns[6].kind, ColumnKind::Duration);
    let r = p.parse("2026-09-29 10:00:01 10.0.0.5 GET /x 200 -").unwrap();
    assert_eq!(get(&p, &r, "time-taken"), None);
    let r = p.parse("2026-09-29 10:00:01 - GET /x 200 15").unwrap();
    assert_eq!(get(&p, &r, "s-ip"), None);
    assert!(p.parse("2026-09-29 10:00:01 too few").is_none());
    check_spans(line, &p.parse(line).unwrap());
}

#[test]
fn log4j_pattern() {
    let spec = ParserSpec::Log4j {
        pattern: "%d{yyyy-MM-dd HH:mm:ss,SSS} %-5p [%t] %c{1} - %m%n".into(),
        kinds: Default::default(),
    };
    let p = compile(spec);
    let names: Vec<_> = p.schema().columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["ts", "level", "thread", "logger", "msg"]);
    let line =
        "2026-09-29 10:00:01,123 INFO  [http-nio-8080-exec-1] c.e.UserService - User 42 logged in";
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "ts"), Some("2026-09-29 10:00:01,123"));
    assert_eq!(get(&p, &r, "level"), Some("INFO"));
    assert_eq!(get(&p, &r, "thread"), Some("http-nio-8080-exec-1"));
    assert_eq!(get(&p, &r, "logger"), Some("c.e.UserService"));
    assert_eq!(get(&p, &r, "msg"), Some("User 42 logged in"));
    let r = p
        .parse("2026-09-29 10:00:01,123 ERROR [main] c.e.Db - boom")
        .unwrap();
    assert_eq!(get(&p, &r, "level"), Some("ERROR"));
    assert!(p.parse("\tat com.example.Foo.bar(Foo.java:12)").is_none());
    check_spans(line, &p.parse(line).unwrap());
}

#[test]
fn log4j_logback_variants() {
    let re =
        log4j_pattern_to_regex("%d{HH:mm:ss.SSS} [%thread] %-5level %logger{36} - %msg%n").unwrap();
    let p = compile(ParserSpec::Regex {
        pattern: re,
        kinds: Default::default(),
    });
    let r = p
        .parse("10:00:01.123 [main] DEBUG org.foo.Bar - hello world")
        .unwrap();
    assert_eq!(get(&p, &r, "ts"), Some("10:00:01.123"));
    assert_eq!(get(&p, &r, "logger"), Some("org.foo.Bar"));
    assert!(log4j_pattern_to_regex("no conversions").is_err());
    assert!(log4j_pattern_to_regex("%").is_err());
    assert!(log4j_pattern_to_regex("%d{unclosed").is_err());
    let re = log4j_pattern_to_regex("%X{user} %m %X{user}").unwrap();
    assert!(re.contains("user_2"));
    // 100% literal
    assert!(log4j_pattern_to_regex("100%% %m").is_ok());
}

#[test]
fn jsonl_nested() {
    let sample = [
        r#"{"time":"2026-09-29T10:00:00Z","msg":"hi","level":"info","http":{"status":500,"method":"GET"},"tags":["a","b"]}"#,
        r#"{"level":"warn","extra_key":true,"http":{"status":200}}"#,
    ];
    let cols = discover_json_columns(&sample);
    assert_eq!(cols[0], "time");
    assert_eq!(cols[1], "level");
    assert_eq!(cols.last().unwrap(), "msg");
    assert!(cols.contains(&"http.status".to_string()));
    let p = compile(ParserSpec::JsonLines {
        columns: cols,
        kinds: Default::default(),
    });
    let r = p.parse(sample[0]).unwrap();
    assert_eq!(get(&p, &r, "http.status"), Some("500"));
    assert_eq!(get(&p, &r, "tags"), Some(r#"["a","b"]"#));
    assert_eq!(get(&p, &r, "level"), Some("info"));
    assert_eq!(get(&p, &r, "extra_key"), None);
    let r = p.parse(sample[1]).unwrap();
    assert_eq!(get(&p, &r, "extra_key"), Some("true"));
    // Unknown keys go to extra
    let p2 = compile(ParserSpec::JsonLines {
        columns: vec!["level".into()],
        kinds: Default::default(),
    });
    let r = p2.parse(sample[0]).unwrap();
    assert_eq!(r.extra_value("http.method"), Some("GET"));
    assert!(p.parse("not json").is_none());
    assert!(p.parse("[1,2]").is_none());
    assert!(p.parse(r#"{"a":"#).is_none());
}

#[test]
fn jsonl_discovery_cap() {
    let line = format!(
        "{{{}}}",
        (0..100)
            .map(|i| format!("\"k{i}\":{i}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(discover_json_columns(&[line]).len(), 64);
}

#[test]
fn logfmt_quotes() {
    let cols = discover_logfmt_columns(&[
        r#"ts=2026-09-29T10:00:00Z level=info msg="hello \"world\"" dur=12ms verbose"#,
    ]);
    assert_eq!(cols[0], "ts");
    assert_eq!(cols[1], "level");
    let p = compile(ParserSpec::Logfmt {
        columns: cols,
        kinds: Default::default(),
    });
    let line = r#"ts=2026-09-29T10:00:00Z level=info msg="hello \"world\"" dur=12ms verbose"#;
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "msg"), Some(r#"hello "world""#));
    assert_eq!(get(&p, &r, "dur"), Some("12ms"));
    assert_eq!(get(&p, &r, "verbose"), Some("true"));
    check_spans(line, &r);
    let line2 = r#"level=warn msg="plain quoted" a="" b=c=d"#;
    let r = p.parse(line2).unwrap();
    assert_eq!(get(&p, &r, "msg"), Some("plain quoted"));
    assert!(r.span(p.schema().find("msg").unwrap()).is_some());
    assert_eq!(r.extra_value("b"), Some("c=d"));
    assert!(p.parse("just some prose here").is_none());
    assert!(p.parse("").is_none());
    assert!(p.parse(r#"k="unterminated"#).is_some());
}

#[test]
fn csv_quotes() {
    let spec = ParserSpec::delimited_from_header_line("id,name,comment", ',');
    let p = compile(spec);
    assert_eq!(p.schema().len(), 3);
    assert!(
        p.parse("id,name,comment").is_none(),
        "header is not a record"
    );
    let line = r#"7,"Smith, John","said ""hi"" twice""#;
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "id"), Some("7"));
    assert_eq!(get(&p, &r, "name"), Some("Smith, John"));
    assert_eq!(get(&p, &r, "comment"), Some(r#"said "hi" twice"#));
    check_spans(line, &r);
    let r = p.parse("8,plain,text").unwrap();
    assert!(r.span(0).is_some(), "unquoted fields keep spans");
    assert!(p.parse("1,2").is_none());
    assert!(p.parse("1,2,3,4").is_none());
    let r = p.parse("1,,3").unwrap();
    assert_eq!(get(&p, &r, "name"), None);
    let r = p.parse("1,2,3\r").unwrap();
    assert_eq!(get(&p, &r, "comment"), Some("3"));
}

#[test]
fn tsv_and_pipe() {
    let p = compile(ParserSpec::Delimited {
        delimiter: '\t',
        quote: '"',
        has_header: false,
        columns: vec!["a".into(), "b".into()],
        kinds: Default::default(),
    });
    let r = p.parse("x\ty z").unwrap();
    assert_eq!(get(&p, &r, "b"), Some("y z"));
    let bad = ParserSpec::Delimited {
        delimiter: ',',
        quote: ',',
        has_header: false,
        columns: vec!["a".into()],
        kinds: Default::default(),
    };
    assert!(bad.compile().is_err());
}

#[test]
fn fixed_width() {
    let p = compile(ParserSpec::FixedWidth {
        columns: vec![
            FixedColumn {
                name: "id".into(),
                start: 0,
                end: Some(5),
            },
            FixedColumn {
                name: "name".into(),
                start: 5,
                end: Some(15),
            },
            FixedColumn {
                name: "rest".into(),
                start: 15,
                end: None,
            },
        ],
    });
    let line = "00042Ünïcode   tail end";
    let r = p.parse(line).unwrap();
    assert_eq!(get(&p, &r, "id"), Some("00042"));
    assert_eq!(get(&p, &r, "name"), Some("Ünïcode"));
    assert_eq!(get(&p, &r, "rest"), Some("tail end"));
    check_spans(line, &r);
    let r = p.parse("00001Bob").unwrap();
    assert_eq!(get(&p, &r, "rest"), None);
    assert!(p.parse("").is_none());
}

#[test]
fn regex_spec_errors() {
    assert!(
        ParserSpec::Regex {
            pattern: "(unclosed".into(),
            kinds: Default::default()
        }
        .compile()
        .is_err()
    );
    assert!(
        ParserSpec::Regex {
            pattern: "no groups".into(),
            kinds: Default::default()
        }
        .compile()
        .is_err()
    );
    let p = compile(ParserSpec::Regex {
        pattern: r"(?P<ts>\S+ \S+) (?P<level>\w+) \[(?P<thread>[^\]]+)\] (?P<msg>.*)".into(),
        kinds: Default::default(),
    });
    let r = p
        .parse("2026-09-29 10:00:00 WARN [main] disk almost full")
        .unwrap();
    assert_eq!(get(&p, &r, "msg"), Some("disk almost full"));
    assert_eq!(p.schema().columns[1].kind, ColumnKind::Level);
}

#[test]
fn spec_serde_roundtrip() {
    let specs = vec![
        ParserSpec::AccessCombined,
        ParserSpec::Syslog5424,
        ParserSpec::JsonLines {
            columns: vec!["a".into()],
            kinds: Default::default(),
        },
        ParserSpec::delimited_from_header_line("a;b", ';'),
    ];
    for s in specs {
        let j = serde_json::to_string(&s).unwrap();
        let back: ParserSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
    let s: ParserSpec = serde_json::from_str(r#"{"type":"syslog3164"}"#).unwrap();
    assert_eq!(s, ParserSpec::Syslog3164);
    let s: ParserSpec = serde_json::from_str(r#"{"type":"delimited","columns":["a"]}"#).unwrap();
    assert!(matches!(s, ParserSpec::Delimited { delimiter: ',', .. }));
}

mod props {
    use super::*;
    use proptest::prelude::*;

    fn all_parsers() -> Vec<Parser> {
        vec![
            compile(ParserSpec::AccessCombined),
            compile(ParserSpec::AccessCommon),
            compile(ParserSpec::Syslog3164),
            compile(ParserSpec::Syslog5424),
            compile(ParserSpec::delimited_from_header_line("a,b,c", ',')),
            compile(ParserSpec::delimited_from_header_line("a\tb", '\t')),
            compile(ParserSpec::JsonLines {
                columns: vec!["a".into(), "b.c".into()],
                kinds: Default::default(),
            }),
            compile(ParserSpec::Logfmt {
                columns: vec!["a".into(), "msg".into()],
                kinds: Default::default(),
            }),
            compile(ParserSpec::w3c_from_fields_line("#Fields: a b c").unwrap()),
            compile(ParserSpec::Log4j {
                pattern: "%d{ISO8601} %-5p [%t] %c{1} - %m%n".into(),
                kinds: Default::default(),
            }),
            compile(ParserSpec::FixedWidth {
                columns: vec![
                    FixedColumn {
                        name: "a".into(),
                        start: 1,
                        end: Some(4),
                    },
                    FixedColumn {
                        name: "b".into(),
                        start: 4,
                        end: None,
                    },
                ],
            }),
        ]
    }

    proptest! {
        #[test]
        fn never_panics_and_spans_are_valid(line in any::<String>()) {
            for p in all_parsers() {
                if let Some(r) = p.parse(&line) {
                    prop_assert_eq!(r.fields.len(), p.schema().len());
                    prop_assert_eq!(r.spans.len(), p.schema().len());
                    let l = line.strip_suffix('\r').unwrap_or(&line);
                    for (i, s) in r.spans.iter().enumerate() {
                        if let Some(s) = s {
                            prop_assert!(s.end <= l.len() && s.start <= s.end);
                            prop_assert!(l.is_char_boundary(s.start) && l.is_char_boundary(s.end));
                            prop_assert_eq!(Some(&l[s.clone()]), r.get(i));
                        }
                    }
                }
                prop_assert_eq!(p.is_match(&line), p.parse(&line).is_some() || p.is_match(&line));
            }
        }

        #[test]
        fn csv_roundtrip_of_quoted_fields(a in "[ -~]{0,12}", b in "[ -~]{0,12}") {
            let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            let line = format!("{},{}", q(&a), q(&b));
            let p = compile(ParserSpec::Delimited {
                delimiter: ',', quote: '"', has_header: false,
                columns: vec!["a".into(), "b".into()], kinds: Default::default(),
            });
            let r = p.parse(&line).unwrap();
            prop_assert_eq!(r.get(0).unwrap_or(""), a.as_str());
            prop_assert_eq!(r.get(1).unwrap_or(""), b.as_str());
        }
    }
}
