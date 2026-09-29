//! Regressions found by `fuzz/fuzz_targets/columns_parsers.rs`.

use oxtail_columns::{FixedColumn, ParserSpec};

/// BUG: `Parser::is_match` strips one trailing `\r` and then calls
/// `Parser::parse`, which strips another, so a line ending in `\r\r` is a
/// record for `parse` but not for `is_match`.
/// Input: `"\r\r"` with a FixedWidth parser. Expected: both agree.
#[test]
fn is_match_agrees_with_parse_on_double_cr() {
    let p = ParserSpec::FixedWidth {
        columns: vec![
            FixedColumn {
                name: "a".into(),
                start: 0,
                end: Some(5),
            },
            FixedColumn {
                name: "b".into(),
                start: 6,
                end: None,
            },
        ],
    }
    .compile()
    .unwrap();
    let line = "\r\r";
    assert_eq!(p.parse(line).is_some(), p.is_match(line));
}
