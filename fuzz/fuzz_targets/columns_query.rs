//! Query language: parse arbitrary text, then evaluate and check the result
//! against a schema and record. Nothing may panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtail_columns::{ParserSpec, Query, Rfc3339Fallback};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let Ok(q) = Query::parse_with_now(&text, 1_800_000_000_000_000_000) else {
        return;
    };
    let _ = q.is_empty();
    let parser = ParserSpec::AccessCombined
        .compile()
        .expect("built-in spec compiles");
    let line =
        r#"10.0.0.1 - bob [29/Sep/2026:10:00:00 +0000] "GET /a?b=c HTTP/1.1" 200 512 "-" "curl/8""#;
    let rec = parser.parse(line);
    let schema = parser.schema();
    let _ = q.check(schema);
    let _ = q.matches(line, rec.as_ref(), Some(schema), &Rfc3339Fallback);
    let _ = q.matches(&text, None, None, &Rfc3339Fallback);
});
