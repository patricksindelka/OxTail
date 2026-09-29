//! Random query and haystack: compile never panics; `find_iter` ranges are in
//! bounds, ordered, non-overlapping and non-empty; `is_match` agrees with them
//! for literals.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use oxtail_search::{CaseMode, Matcher, Query, QueryKind};

#[derive(Debug, Arbitrary)]
struct Input {
    pattern: String,
    regex: bool,
    case: u8,
    whole_word: bool,
    haystack: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let q = Query {
        pattern: input.pattern.clone(),
        kind: if input.regex {
            QueryKind::Regex
        } else {
            QueryKind::Literal
        },
        case: match input.case % 3 {
            0 => CaseMode::Smart,
            1 => CaseMode::Sensitive,
            _ => CaseMode::Insensitive,
        },
        whole_word: input.whole_word,
    };
    let Ok(m) = Matcher::compile(&q) else { return };
    let hay = &input.haystack;
    let ranges: Vec<_> = m.find_iter(hay).collect();
    let mut prev_end = 0;
    for r in &ranges {
        assert!(r.start < r.end, "empty range {r:?}");
        assert!(
            r.end <= hay.len(),
            "range {r:?} out of bounds {}",
            hay.len()
        );
        assert!(r.start >= prev_end, "overlapping ranges");
        prev_end = r.end;
    }
    let matched = m.is_match(hay);
    if !ranges.is_empty() {
        assert!(matched, "find_iter found {ranges:?} but is_match is false");
    }
    if !input.regex && !matched {
        assert!(ranges.is_empty());
    }
});
