//! Big-file smoke test (ignored by default): generates a ~2 GB log in a temp
//! directory, opens it with `Document`, and prints timings for the first
//! `Tail{50}` lines, the full index and a literal `SearchJob`.
//!
//! ```text
//! cargo test --release -p oxtail-search --test torture_bigfile -- --ignored --nocapture
//! ```
//!
//! `OXTAIL_BIG_MB` changes the size (default 2048); `OXTAIL_BIG_DIR` the
//! directory (default: the system temp dir). The file is removed afterwards.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::{DocEvent, Document, LineRequest, OpenOptions, RequestId};
use oxtail_search::{CaseMode, Matcher, Query, SearchJob, SearchOptions};

const NEEDLE: &str = "needle-7f3a91";
const NEEDLE_EVERY: u64 = 250_000;
const LIMIT: Duration = Duration::from_secs(300);

const METHODS: [&str; 4] = ["GET", "POST", "PUT", "DELETE"];
const PATHS: [&str; 6] = [
    "/",
    "/api/v1/users",
    "/static/app.js",
    "/login",
    "/api/v1/orders/42",
    "/healthz",
];
const STATUS: [u16; 6] = [200, 200, 200, 301, 404, 500];

/// Removes the generated directory even if an assertion fails.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes nginx-style lines until `target` bytes; returns `(lines, needles)`.
fn generate(path: &std::path::Path, target: u64) -> (u64, u64) {
    let mut w = BufWriter::with_capacity(8 << 20, File::create(path).unwrap());
    let (mut bytes, mut n, mut needles) = (0u64, 0u64, 0u64);
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut line = String::with_capacity(256);
    while bytes < target {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        line.clear();
        use std::fmt::Write as _;
        let _ = write!(
            line,
            "10.{}.{}.{} - - [29/Sep/2026:10:{:02}:{:02} +0000] \"{} {} HTTP/1.1\" {} {} \"-\" \"Mozilla/5.0 (X11; Linux x86_64) req={:016x}\"",
            r & 0xff,
            (r >> 8) & 0xff,
            (r >> 16) & 0xff,
            (r >> 24) % 60,
            (r >> 30) % 60,
            METHODS[(r >> 36) as usize % METHODS.len()],
            PATHS[(r >> 40) as usize % PATHS.len()],
            STATUS[(r >> 44) as usize % STATUS.len()],
            (r >> 48) & 0xffff,
            r,
        );
        if n % NEEDLE_EVERY == NEEDLE_EVERY - 1 {
            line.push(' ');
            line.push_str(NEEDLE);
            needles += 1;
        }
        line.push('\n');
        w.write_all(line.as_bytes()).unwrap();
        bytes += line.len() as u64;
        n += 1;
    }
    w.flush().unwrap();
    (n, needles)
}

#[test]
#[ignore = "generates a ~2 GB file; run with --ignored --nocapture (preferably --release)"]
fn big_file_open_index_and_search() {
    let mb: u64 = std::env::var("OXTAIL_BIG_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2048);
    let base = std::env::var_os("OXTAIL_BIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("oxtail-bigfile-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("big.log");

    let t = Instant::now();
    let (lines, needles) = generate(&path, mb << 20);
    let size = fs::metadata(&path).unwrap().len();
    eprintln!(
        "generate: {lines} lines, {:.2} GiB in {:.1?}",
        size as f64 / (1u64 << 30) as f64,
        t.elapsed()
    );

    // Open and wait for the first `Lines` (the automatic tail-50 result).
    let t = Instant::now();
    let doc = Document::open(
        &path,
        OpenOptions {
            start_at_tail: Some(50),
            follow: false,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let opened = t.elapsed();
    let first = loop {
        let left = LIMIT.saturating_sub(t.elapsed());
        match doc
            .events()
            .recv_timeout(left)
            .expect("no Lines within limit")
        {
            DocEvent::Lines { id, lines, .. } if id == RequestId::INITIAL => break lines,
            _ => {}
        }
    };
    let first_lines = t.elapsed();
    assert_eq!(first.len(), 50);
    assert!(first[49].text.contains("HTTP/1.1"));
    eprintln!("open(): {opened:.1?}; first Lines (Tail 50): {first_lines:.1?}");

    // A second Tail request while (or after) indexing runs.
    let t2 = Instant::now();
    let id = doc.request_lines(LineRequest::Tail { count: 50 });
    loop {
        match doc.events().recv_timeout(LIMIT).expect("tail answer") {
            DocEvent::Lines { id: i, .. } if i == id => break,
            _ => {}
        }
    }
    eprintln!("Tail{{50}} request: {:.1?}", t2.elapsed());

    // Full index.
    loop {
        let s = doc.snapshot();
        if s.indexed_bytes == s.utf8_len && s.lines.exact && s.utf8_len == size {
            eprintln!(
                "full index: {:.2?} (from open) = {:.0} MB/s; {} lines, {} checkpoints' worth of memory",
                t.elapsed(),
                size as f64 / 1e6 / t.elapsed().as_secs_f64(),
                s.lines.known,
                s.utf8_len / (64 * 1024)
            );
            assert_eq!(s.lines.known, lines);
            break;
        }
        assert!(t.elapsed() < LIMIT, "indexing did not finish: {s:?}");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Literal search over the whole file.
    for (label, case) in [
        ("literal, case-sensitive", CaseMode::Sensitive),
        ("literal, case-insensitive", CaseMode::Insensitive),
    ] {
        let m = Matcher::compile(&Query::literal(NEEDLE).with_case(case)).unwrap();
        let t3 = Instant::now();
        let h = SearchJob::start(doc.source(), size, Arc::new(m), SearchOptions::default());
        loop {
            if h.poll().done {
                break;
            }
            assert!(t3.elapsed() < LIMIT, "search did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        let took = t3.elapsed();
        let found = h.matches().len() as u64;
        eprintln!(
            "search ({label}): {took:.2?} = {:.0} MB/s, {found} matches",
            size as f64 / 1e6 / took.as_secs_f64()
        );
        assert_eq!(found, needles);
        assert_eq!(h.poll().io_errors, 0);
    }
}
