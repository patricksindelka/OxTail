//! Random rules (built through `arbitrary`) plus a random line: compiling and
//! highlighting never panic and the resulting spans are valid.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use oxtail_highlight::{ColumnOp, CompiledRules, Rule, RuleMatcher, Scope};

#[derive(Debug, Arbitrary)]
enum M {
    Literal(String, bool),
    Regex(String, bool),
    Column(String, u8, String),
}

#[derive(Debug, Arbitrary)]
struct R {
    matcher: M,
    scope: u8,
    group: u8,
    scope_column: String,
    priority: i8,
    enabled: bool,
}

#[derive(Debug, Arbitrary)]
struct Input {
    rules: Vec<R>,
    line: String,
    columns: Vec<(String, u8, u8)>,
}

const OPS: [ColumnOp; 8] = [
    ColumnOp::Eq,
    ColumnOp::Ne,
    ColumnOp::Contains,
    ColumnOp::Regex,
    ColumnOp::Gt,
    ColumnOp::Ge,
    ColumnOp::Lt,
    ColumnOp::Le,
];

fuzz_target!(|input: Input| {
    let rules: Vec<Rule> = input
        .rules
        .into_iter()
        .take(12)
        .map(|r| {
            let matcher = match r.matcher {
                M::Literal(text, case_sensitive) => RuleMatcher::Literal {
                    text,
                    case_sensitive,
                },
                M::Regex(pattern, case_sensitive) => RuleMatcher::Regex {
                    pattern,
                    case_sensitive,
                },
                M::Column(column, op, value) => RuleMatcher::Column {
                    column,
                    op: OPS[usize::from(op) % OPS.len()],
                    value,
                },
            };
            let mut rule = Rule::new("fuzz", matcher);
            rule.enabled = r.enabled;
            rule.priority = i32::from(r.priority);
            rule.scope = match r.scope % 4 {
                0 => Scope::Line,
                1 => Scope::Match,
                2 => Scope::Group(usize::from(r.group)),
                _ => Scope::Column(r.scope_column),
            };
            rule
        })
        .collect();
    let Ok(compiled) = CompiledRules::compile(&rules) else {
        return;
    };
    let line = &input.line;

    // Column ranges must be valid for the line (they come from a parser).
    let cols: Vec<(&str, std::ops::Range<usize>)> = input
        .columns
        .iter()
        .take(8)
        .filter_map(|(name, a, b)| {
            let (a, b) = (usize::from(*a), usize::from(*b));
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            (b <= line.len() && line.is_char_boundary(a) && line.is_char_boundary(b))
                .then(|| (name.as_str(), a..b))
        })
        .collect();

    for columns in [None, Some(cols.as_slice())] {
        let h = compiled.highlight(line, columns);
        let mut prev_end = 0;
        for s in &h.spans {
            assert!(s.range.start < s.range.end, "empty span");
            assert!(s.range.start >= prev_end, "unsorted or overlapping spans");
            assert!(s.range.end <= line.len(), "span out of bounds");
            assert!(
                line.is_char_boundary(s.range.start) && line.is_char_boundary(s.range.end),
                "span not on char boundary"
            );
            prev_end = s.range.end;
        }
        assert!(h.matched_rules.windows(2).all(|w| w[0] < w[1]));
        assert!(h.matched_rules.iter().all(|i| *i < rules.len()));
    }
});
