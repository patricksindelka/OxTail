//! Per-line parse cost. Target: well under 2 microseconds for a typical
//! ~200-byte delimited, logfmt or regex line.

use criterion::{Criterion, criterion_group, criterion_main};
use oxtail_columns::{ParserSpec, Query, Rfc3339Fallback, discover_logfmt_columns};
use std::hint::black_box;

fn bench_parsers(c: &mut Criterion) {
    let csv_line = "2026-09-29T10:00:01.123Z,INFO,web-01,GET,/api/v1/users/12345/orders,200,1532,\
        12.5,\"Mozilla/5.0 (X11; Linux x86_64)\",req-8f3a2c1d-9b7e-4c11-a0f2-1234567890ab,us-east-1,ok,none";
    let csv = ParserSpec::delimited_from_header_line(
        "ts,level,host,method,path,status,size,ms,agent,request_id,region,result,error",
        ',',
    )
    .compile()
    .unwrap();

    let csv_quoted = "2026-09-29T10:00:01.123Z,INFO,web-01,GET,\"/api/v1/users/12345/orders\",200,1532,\
        12.5,\"Mozilla/5.0 (X11; Linux x86_64)\",req-8f3a2c1d-9b7e-4c11-a0f2-1234567890ab,us-east-1,ok,none";

    let logfmt_line = "ts=2026-09-29T10:00:01.123Z level=info host=web-01 method=GET \
        path=/api/v1/users/12345/orders status=200 size=1532 dur=12.5ms \
        msg=\"request completed successfully\" req=8f3a2c1d-9b7e-4c11-a0f2 region=us-east-1";
    let logfmt = ParserSpec::Logfmt {
        columns: discover_logfmt_columns(&[logfmt_line]),
        kinds: Default::default(),
    }
    .compile()
    .unwrap();

    let l4j_line = "2026-09-29 10:00:01,123 INFO  [http-nio-8080-exec-17] c.example.orders.OrderService - \
        Order 12345 for user 987 confirmed in 12 ms; warehouse=east-1 items=3 total=59.90 EUR";
    let l4j = ParserSpec::Log4j {
        pattern: "%d{yyyy-MM-dd HH:mm:ss,SSS} %-5p [%t] %c{1} - %m%n".into(),
        kinds: Default::default(),
    }
    .compile()
    .unwrap();

    let nginx_line = r#"93.184.216.34 - alice [29/Sep/2026:10:00:01 +0000] "GET /api/v1/users/12345/orders?page=2&sort=asc HTTP/1.1" 200 1532 "https://example.com/dashboard" "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36""#;
    let nginx = ParserSpec::AccessCombined.compile().unwrap();

    let json_line = r#"{"ts":"2026-09-29T10:00:01.123Z","level":"info","msg":"request completed","http":{"method":"GET","path":"/api/v1/users/12345/orders","status":200},"dur_ms":12.5,"req":"8f3a2c1d-9b7e-4c11-a0f2","region":"us-east-1"}"#;
    let json = ParserSpec::JsonLines {
        columns: oxtail_columns::discover_json_columns(&[json_line]),
        kinds: Default::default(),
    }
    .compile()
    .unwrap();

    let mut g = c.benchmark_group("parse_line");
    g.bench_function("delimited_plain", |b| {
        b.iter(|| black_box(csv.parse(black_box(csv_line))))
    });
    g.bench_function("delimited_quoted", |b| {
        b.iter(|| black_box(csv.parse(black_box(csv_quoted))))
    });
    g.bench_function("logfmt", |b| {
        b.iter(|| black_box(logfmt.parse(black_box(logfmt_line))))
    });
    g.bench_function("regex_log4j", |b| {
        b.iter(|| black_box(l4j.parse(black_box(l4j_line))))
    });
    g.bench_function("nginx_combined", |b| {
        b.iter(|| black_box(nginx.parse(black_box(nginx_line))))
    });
    g.bench_function("json_lines", |b| {
        b.iter(|| black_box(json.parse(black_box(json_line))))
    });
    g.finish();

    let q = Query::parse("status>=500 -path:/health level:(ERROR|FATAL) timeout").unwrap();
    let rec = nginx.parse(nginx_line).unwrap();
    let schema = nginx.schema().clone();
    c.bench_function("query_match", |b| {
        b.iter(|| {
            black_box(q.matches(
                black_box(nginx_line),
                Some(&rec),
                Some(&schema),
                &Rfc3339Fallback,
            ))
        })
    });
}

criterion_group!(benches, bench_parsers);
criterion_main!(benches);
