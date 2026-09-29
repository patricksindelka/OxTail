//! Index throughput on a generated ~200 MB in-memory log.
//!
//! `scanner_feed` measures the pure newline-counting/checkpoint path;
//! `extend_mem_source` adds the `ReadAt` copy that `LineIndex::extend` does.
//! Target: >= 1 GB/s in release.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oxtail_core::MemSource;
use oxtail_core::index::{DEFAULT_SPACING, LineIndex};

fn generate(bytes: usize) -> Vec<u8> {
    let mut rng = fastrand::Rng::with_seed(42);
    let mut out = Vec::with_capacity(bytes + 512);
    let mut n = 0u64;
    while out.len() < bytes {
        let len = 40 + rng.usize(0..160);
        out.extend_from_slice(
            format!(
                "2024-05-01T12:00:{:02}Z INFO request id={n} path=/api/v1/items status=200 ",
                n % 60
            )
            .as_bytes(),
        );
        while out.len() % 1024 != 0 && rng.u8(..) != 0 && out.len() < bytes {
            out.push(b'a' + rng.u8(0..26));
            if out.len() % len == 0 {
                break;
            }
        }
        out.push(b'\n');
        n += 1;
    }
    out
}

fn bench_index(c: &mut Criterion) {
    let data = generate(200 * 1024 * 1024);
    let mut g = c.benchmark_group("index");
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.sample_size(10);

    g.bench_function("scanner_feed", |b| {
        b.iter(|| {
            let idx = LineIndex::with_spacing(DEFAULT_SPACING);
            let mut sc = idx.scanner();
            let mut cps = Vec::new();
            for chunk in data.chunks(4 * 1024 * 1024) {
                sc.feed(black_box(chunk), &mut cps);
            }
            black_box((sc.position(), cps.len()))
        })
    });

    let src = MemSource::new(data.clone());
    g.bench_function("extend_mem_source", |b| {
        b.iter(|| {
            let mut idx = LineIndex::new();
            idx.extend(&src, data.len() as u64, &|| false).unwrap();
            black_box(idx.total_lines())
        })
    });
    g.finish();
}

criterion_group!(benches, bench_index);
criterion_main!(benches);
