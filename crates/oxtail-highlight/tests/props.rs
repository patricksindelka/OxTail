//! Property tests: nothing panics, and spans are always well-formed.

use std::ops::Range;
use std::sync::OnceLock;

use oxtail_highlight::{
    ColumnOp, CompiledRules, Rule, RuleMatcher, Scope, Style, StyledSpan, ansi, presets,
};
use proptest::prelude::*;

fn assert_well_formed(text: &str, spans: &[StyledSpan]) -> Result<(), TestCaseError> {
    let mut prev_end = 0;
    for s in spans {
        prop_assert!(s.range.start < s.range.end, "empty span {:?}", s.range);
        prop_assert!(
            s.range.start >= prev_end,
            "unsorted or overlapping: {spans:?}"
        );
        prop_assert!(s.range.end <= text.len(), "out of bounds");
        prop_assert!(
            text.is_char_boundary(s.range.start),
            "start not on a char boundary"
        );
        prop_assert!(
            text.is_char_boundary(s.range.end),
            "end not on a char boundary"
        );
        prop_assert!(s.style != Style::default(), "default-styled span");
        prev_end = s.range.end;
    }
    Ok(())
}

fn rules() -> &'static CompiledRules {
    static R: OnceLock<CompiledRules> = OnceLock::new();
    R.get_or_init(|| {
        let mut all = presets::default_rules();
        all.push(
            Rule::new(
                "slow",
                RuleMatcher::Column {
                    column: "a".into(),
                    op: ColumnOp::Gt,
                    value: "10".into(),
                },
            )
            .scoped(Scope::Column("b".into()))
            .styled(Style::default().bold()),
        );
        all.push(Rule::literal("lit", "\u{e9}t\u{e9}").styled(Style::default().italic()));
        all.push(
            Rule::regex("grp", r"(\w)(\d)")
                .scoped(Scope::Group(2))
                .styled(Style::default().dim()),
        );
        all.push(Rule::regex("empty", "x*").styled(Style::default().underline()));
        CompiledRules::compile(&all).unwrap()
    })
}

/// Text that looks like logs: ASCII, digits, punctuation, multi-byte chars.
fn logish() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            4 => "[a-zA-Z]{1,6}",
            3 => "[0-9]{1,5}",
            2 => Just(" ".to_owned()),
            2 => prop::sample::select(vec![
                "ERROR", "warn", "[E]", " W ", "2026-09-29T12:00:01Z", "10.0.0.1", "::1",
                "http://x.y/z", "/var/log/a.log", "GET /", "\"200", "at a.B(C.java:1)",
                "\u{e9}t\u{e9}", "\u{1f600}", "\u{4e2d}\u{6587}", ":", "-", ".", "\t",
            ]).prop_map(str::to_owned),
        ],
        0..30,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn highlight_never_panics_and_spans_are_well_formed(line in prop_oneof![logish(), any::<String>()]) {
        let h = rules().highlight(&line, None);
        assert_well_formed(&line, &h.spans)?;
        prop_assert!(h.matched_rules.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn highlight_with_arbitrary_column_ranges(
        line in logish(),
        cols in proptest::collection::vec(("[ab]", 0usize..80, 0usize..80), 0..4),
    ) {
        let cols: Vec<(&str, Range<usize>)> = cols
            .iter()
            .map(|(n, a, b)| (n.as_str(), *a..*b))
            .collect();
        let h = rules().highlight(&line, Some(&cols));
        assert_well_formed(&line, &h.spans)?;
    }

    #[test]
    fn ansi_parse_never_panics_and_spans_are_well_formed(
        line in prop_oneof![
            any::<String>(),
            proptest::collection::vec(
                prop_oneof![
                    Just("\x1b".to_owned()), Just("[".to_owned()), Just("]".to_owned()),
                    Just(";".to_owned()), Just(":".to_owned()), Just("m".to_owned()),
                    Just("\x07".to_owned()), Just("\\".to_owned()), Just("\u{e9}".to_owned()),
                    "[0-9]{1,3}", "[a-zA-Z ]{1,4}",
                ],
                0..40,
            ).prop_map(|v| v.concat()),
        ],
    ) {
        let (text, spans) = ansi::parse(&line);
        assert_well_formed(&text, &spans)?;
        prop_assert!(!text.contains('\x1b'));
    }

    #[test]
    fn ansi_parse_of_escape_free_text_is_identity(line in "[^\x1b]*") {
        let (text, spans) = ansi::parse(&line);
        prop_assert_eq!(text, line);
        prop_assert!(spans.is_empty());
    }
}
