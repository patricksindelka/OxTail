//! Search throughput on a generated ~200 MB in-memory log.

use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oxtail_core::MemSource;
use oxtail_search::{CaseMode, Matcher, Query, SearchJob, SearchOptions};

const TARGET: usize = 200 * 1024 * 1024;

fn generate() -> Vec<u8> {
    let levels = ["INFO", "INFO", "INFO", "DEBUG", "WARN", "TRACE"];
    let mut out = Vec::with_capacity(TARGET + 512);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut i = 0u64;
    while out.len() < TARGET {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let level = levels[(x % levels.len() as u64) as usize];
        // Roughly 1 line in 5000 carries a rare error marker.
        let rare = if x.is_multiple_of(5000) {
            "Connection RESET by peer code=503"
        } else {
            "request completed"
        };
        let line = format!(
            "2026-09-29T12:{:02}:{:02}.{:03}Z {level} [worker-{}] {rare} id={} bytes={}\n",
            (i / 60) % 60,
            i % 60,
            x % 1000,
            x % 16,
            x >> 20,
            x % 65536,
        );
        out.extend_from_slice(line.as_bytes());
        i += 1;
    }
    out
}

fn run(src: &Arc<MemSource>, len: u64, q: &Query) -> u64 {
    let m = Matcher::compile(q).expect("valid query");
    let h = SearchJob::start(src.clone(), len, Arc::new(m), SearchOptions::default());
    while !h.poll().done {
        std::thread::yield_now();
    }
    h.poll().matches_found
}

fn bench(c: &mut Criterion) {
    let data = generate();
    let len = data.len() as u64;
    let src = Arc::new(MemSource::new(data));
    let mut g = c.benchmark_group("search_200MB");
    g.throughput(Throughput::Bytes(len));
    g.sample_size(10).measurement_time(Duration::from_secs(8));
    let cases = [
        (
            "literal",
            Query::literal("RESET by peer").with_case(CaseMode::Sensitive),
        ),
        (
            "literal_ignore_case",
            Query::literal("reset by peer").with_case(CaseMode::Insensitive),
        ),
        (
            "regex",
            Query::regex(r"code=5\d\d").with_case(CaseMode::Sensitive),
        ),
        (
            "regex_end_anchored",
            Query::regex(r"bytes=1\d\d\d\d$").with_case(CaseMode::Sensitive),
        ),
    ];
    for (name, q) in &cases {
        g.bench_function(*name, |b| b.iter(|| run(&src, len, q)));
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
