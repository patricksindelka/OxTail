//! Property tests: the engine against naive per-line reference code.

use std::sync::Arc;
use std::time::{Duration, Instant};

use oxtail_core::MemSource;
use oxtail_search::{
    CaseMode, Filter, FilterJob, FilterStack, LinePredicate, Matcher, Query, SearchHandle,
    SearchJob, SearchOptions,
};
use proptest::prelude::*;
use regex::bytes::Regex;

/// `(start, line without terminator)` for every line, like the byte model says.
fn naive_lines(data: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let nl = data[start..].iter().position(|&b| b == b'\n');
        let end = nl.map_or(data.len(), |i| start + i);
        let mut line = &data[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        out.push((start as u64, line));
        start = end + 1;
    }
    out
}

fn wait(h: &SearchHandle) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.poll().done {
        assert!(Instant::now() < deadline, "job did not finish in 10s");
        std::thread::yield_now();
    }
}

fn run_search(data: &[u8], pred: Arc<dyn LinePredicate>, chunk_size: u64, hint: u64) -> Vec<u64> {
    let h = SearchJob::start(
        Arc::new(MemSource::new(data.to_vec())),
        data.len() as u64,
        pred,
        SearchOptions {
            chunk_size,
            viewport_hint: hint,
            ..Default::default()
        },
    );
    wait(&h);
    h.matches().iter().collect()
}

fn data_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(
        prop_oneof![
            8 => Just(b'a'),
            6 => Just(b'b'),
            2 => Just(b'A'),
            2 => Just(b' '),
            3 => Just(b'\n'),
            1 => Just(b'\r'),
            1 => Just(b'_'),
            1 => Just(0xC3u8),
            1 => Just(0xA9u8),
        ],
        0..300,
    )
}

/// Like `data_strategy` but always valid UTF-8 (whole-word rules are only
/// defined for valid text).
fn valid_data_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(
        prop_oneof![
            8 => Just("a"),
            6 => Just("b"),
            2 => Just("A"),
            2 => Just(" "),
            3 => Just("\n"),
            1 => Just("\r"),
            1 => Just("_"),
            1 => Just("\u{e9}"),
        ],
        0..300,
    )
    .prop_map(|v| v.concat().into_bytes())
}

fn needle_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![Just('a'), Just('b'), Just('A'), Just(' ')],
        1..4,
    )
    .prop_map(|v| v.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn literal_search_matches_naive(
        data in data_strategy(),
        needle in needle_strategy(),
        insensitive in any::<bool>(),
        cs in 1u64..40,
        hint in 0u64..400,
    ) {
        let case = if insensitive { CaseMode::Insensitive } else { CaseMode::Sensitive };
        let m = Matcher::compile(&Query::literal(needle.clone()).with_case(case)).unwrap();
        let got = run_search(&data, Arc::new(m), cs, hint);
        let expect: Vec<u64> = naive_lines(&data)
            .into_iter()
            .filter(|(_, l)| {
                if insensitive {
                    let l = l.to_ascii_lowercase();
                    let n = needle.to_ascii_lowercase();
                    l.windows(n.len()).any(|w| w == n.as_bytes())
                } else {
                    l.windows(needle.len()).any(|w| w == needle.as_bytes())
                }
            })
            .map(|(o, _)| o)
            .collect();
        prop_assert_eq!(got, expect);
    }

    #[test]
    fn regex_search_matches_naive(
        data in data_strategy(),
        pat in prop_oneof![
            Just("a+b"), Just(r"a\s*b"), Just("^a"), Just("b$"), Just("a.b"),
            Just("[^a]b"), Just(r"\bab\b"), Just("(?i)ab"), Just("^$"), Just("a|b$"),
            Just(r"\s"), Just(r"a\s+"), Just(""), Just("x*"),
        ],
        cs in 1u64..40,
    ) {
        if pat.is_empty() {
            prop_assert!(Matcher::compile(&Query::regex(pat)).is_err());
            return Ok(());
        }
        let m = Matcher::compile(&Query::regex(pat).with_case(CaseMode::Sensitive)).unwrap();
        let re = Regex::new(pat).unwrap();
        let got = run_search(&data, Arc::new(m), cs, 0);
        let expect: Vec<u64> = naive_lines(&data)
            .into_iter()
            .filter(|(_, l)| re.is_match(l))
            .map(|(o, _)| o)
            .collect();
        prop_assert_eq!(got, expect);
    }

    #[test]
    fn whole_word_literal_matches_naive(
        data in valid_data_strategy(),
        needle in prop_oneof![Just("ab"), Just("a"), Just("b"), Just("aab")],
        cs in 1u64..40,
    ) {
        let q = Query::literal(needle).with_case(CaseMode::Sensitive).with_whole_word(true);
        let m = Matcher::compile(&q).unwrap();
        let re = Regex::new(&format!(r"\b{needle}\b")).unwrap();
        let got = run_search(&data, Arc::new(m), cs, 0);
        let expect: Vec<u64> = naive_lines(&data)
            .into_iter()
            .filter(|(_, l)| re.is_match(l))
            .map(|(o, _)| o)
            .collect();
        prop_assert_eq!(got, expect);
    }

    #[test]
    fn filter_stack_with_context_matches_naive(
        data in data_strategy(),
        inc in needle_strategy(),
        exc in proptest::option::of(needle_strategy()),
        before in 0u32..4,
        after in 0u32..4,
        cs in 1u64..40,
    ) {
        let mk = |p: &str| -> Arc<dyn LinePredicate> {
            Arc::new(Matcher::compile(&Query::literal(p).with_case(CaseMode::Sensitive)).unwrap())
        };
        let mut stack = FilterStack::new().with(Filter::include(mk(&inc))).with_context(before, after);
        if let Some(e) = &exc {
            stack = stack.with(Filter::exclude(mk(e)));
        }
        let h = FilterJob::start(
            Arc::new(MemSource::new(data.clone())),
            data.len() as u64,
            stack,
            SearchOptions { chunk_size: cs, ..Default::default() },
        );
        wait(&h);
        let got: Vec<(u64, bool)> = h.matches().iter_entries().collect();

        let lines = naive_lines(&data);
        let contains = |l: &[u8], n: &str| l.windows(n.len()).any(|w| w == n.as_bytes());
        let pass: Vec<bool> = lines
            .iter()
            .map(|(_, l)| contains(l, &inc) && !exc.as_deref().is_some_and(|e| contains(l, e)))
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
        prop_assert_eq!(got, expect);
    }

    #[test]
    fn extend_equals_fresh_scan(
        data in data_strategy(),
        split in 0usize..300,
        needle in needle_strategy(),
        cs in 1u64..40,
    ) {
        let split = split.min(data.len());
        let src = Arc::new(MemSource::new(data[..split].to_vec()));
        let pred: Arc<dyn LinePredicate> =
            Arc::new(Matcher::compile(&Query::literal(needle).with_case(CaseMode::Sensitive)).unwrap());
        let h = SearchJob::start(
            src.clone(),
            split as u64,
            pred.clone(),
            SearchOptions { chunk_size: cs, ..Default::default() },
        );
        wait(&h);
        src.append(&data[split..]);
        h.extend(data.len() as u64);
        wait(&h);
        let got: Vec<u64> = h.matches().iter().collect();
        prop_assert_eq!(got, run_search(&data, pred, cs, 0));
    }
}
