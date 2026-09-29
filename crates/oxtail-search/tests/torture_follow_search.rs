//! Torture test for jobs over a growing source: a writer appends random-sized
//! chunks (often ending mid-line, sometimes CRLF) to a `MemSource` and calls
//! `extend` after each append while a reader thread hammers the handle. The
//! final `MatchSet` must equal a naive filter over the final content.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use oxtail_core::MemSource;
use oxtail_search::{
    CaseMode, Filter, FilterJob, FilterStack, LinePredicate, Lookup, Matcher, Query, SearchHandle,
    SearchJob, SearchOptions,
};

/// xorshift64*: the crate has no RNG dev-dependency and the test needs none.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "alpha", "beta", "ERROR", "warn", "info", "timeout", "user=42", "GET /x", "ok", "retry",
];

fn gen_chunk(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.below(12) {
        for _ in 0..rng.below(6) {
            out.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
            out.push(b' ');
        }
        match rng.below(10) {
            0 => out.extend_from_slice(b"\r\n"),
            1 => {} // the line keeps growing into the next chunk
            _ => out.push(b'\n'),
        }
    }
    out
}

fn naive_lines(data: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let end = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |i| start + i);
        let mut line = &data[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        out.push((start as u64, line));
        start = end + 1;
    }
    out
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn wait_done(h: &SearchHandle) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.poll().done {
        assert!(Instant::now() < deadline, "job did not finish within 10 s");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Runs `writer` (which appends and extends) while a reader thread polls the
/// handle, then waits for the job and returns the final data.
fn drive(h: Arc<SearchHandle>, src: Arc<MemSource>, seed: u64, appends: usize) -> Vec<u8> {
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (h, stop) = (h.clone(), stop.clone());
        thread::spawn(move || {
            let mut queries = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let set = h.matches();
                let offs: Vec<u64> = set.iter().collect();
                assert!(offs.windows(2).all(|w| w[0] < w[1]), "unsorted match set");
                let s = h.poll();
                assert!((0.0..=1.0).contains(&s.progress));
                assert_eq!(s.io_errors, 0);
                if let Lookup::Found(o) = h.next_after(0) {
                    assert!(o < h.end() + 1);
                }
                let _ = h.prev_before(u64::MAX);
                queries += 1;
                thread::yield_now();
            }
            queries
        })
    };

    let mut rng = Rng(seed | 1);
    for _ in 0..appends {
        src.append(&gen_chunk(&mut rng));
        // `extend` may be called with the current length, however it fell
        // relative to the last scan.
        h.extend(oxtail_core::ReadAt::len(&*src).unwrap());
        if rng.below(8) == 0 {
            thread::yield_now();
        }
    }
    // Final extend so the job sees everything, then wait.
    let end = oxtail_core::ReadAt::len(&*src).unwrap();
    h.extend(end);
    wait_done(&h);
    stop.store(true, Ordering::Relaxed);
    let queries = reader.join().expect("reader thread panicked");
    eprintln!("torture_follow_search: seed {seed}, {appends} appends, {queries} reader polls");

    let mut data = vec![0u8; end as usize];
    oxtail_core::ReadAt::read_exact_at(&*src, 0, &mut data).unwrap();
    data
}

fn stable_seed(i: u64) -> u64 {
    0x9E37_79B9_7F4A_7C15u64.wrapping_mul(i + 1)
}

#[test]
fn filter_over_growing_source_equals_naive() {
    for (i, chunk_size) in [1u64, 7, 64, 500, 4096].into_iter().enumerate() {
        let seed = stable_seed(i as u64);
        let src = Arc::new(MemSource::new(Vec::new()));
        let mk = |p: &str| -> Arc<dyn LinePredicate> {
            Arc::new(Matcher::compile(&Query::literal(p).with_case(CaseMode::Sensitive)).unwrap())
        };
        let (before, after) = ((seed % 3) as u32, ((seed >> 3) % 3) as u32);
        let stack = FilterStack::new()
            .with(Filter::include(mk("ERROR")))
            .with(Filter::exclude(mk("retry")))
            .with_context(before, after);
        let h = Arc::new(FilterJob::start(
            src.clone(),
            0,
            stack,
            SearchOptions {
                chunk_size,
                ..Default::default()
            },
        ));
        let data = drive(h.clone(), src, seed, 300);

        let lines = naive_lines(&data);
        let pass: Vec<bool> = lines
            .iter()
            .map(|(_, l)| contains(l, "ERROR") && !contains(l, "retry"))
            .collect();
        let mut expect = Vec::new();
        for (i, (off, _)) in lines.iter().enumerate() {
            let lo = i.saturating_sub(after as usize);
            let hi = (i + before as usize).min(lines.len() - 1);
            if pass[i] {
                expect.push((*off, false));
            } else if pass[lo..=hi].iter().any(|&p| p) {
                expect.push((*off, true));
            }
        }
        let got: Vec<(u64, bool)> = h.matches().iter_entries().collect();
        assert_eq!(got, expect, "chunk_size {chunk_size}, seed {seed}");
        assert!(!h.poll().truncated);
    }
}

#[test]
fn search_over_growing_source_equals_naive() {
    for (i, chunk_size) in [3u64, 100, 1000].into_iter().enumerate() {
        let seed = stable_seed(100 + i as u64);
        let src = Arc::new(MemSource::new(Vec::new()));
        let m = Matcher::compile(&Query::regex(r"timeout|user=\d+").with_case(CaseMode::Sensitive))
            .unwrap();
        let re = regex::bytes::Regex::new(r"timeout|user=\d+").unwrap();
        let h = Arc::new(SearchJob::start(
            src.clone(),
            0,
            Arc::new(m),
            SearchOptions {
                chunk_size,
                ..Default::default()
            },
        ));
        let data = drive(h.clone(), src, seed, 300);
        let expect: Vec<u64> = naive_lines(&data)
            .into_iter()
            .filter(|(_, l)| re.is_match(l))
            .map(|(o, _)| o)
            .collect();
        let got: Vec<u64> = h.matches().iter().collect();
        assert_eq!(got, expect, "chunk_size {chunk_size}, seed {seed}");
    }
}
