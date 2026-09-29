//! "Choose parser": builds a `ParserSpec` from what the user typed in the
//! chooser window (a regex with named groups, a log4j layout pattern, a
//! delimiter, or "discover the keys from the first lines"). Pure, so the
//! error messages are unit-tested; the window itself is in [`crate::colui`].

use oxtail_columns::{
    ParserSpec, discover_json_columns, discover_logfmt_columns, log4j_pattern_to_regex,
};

/// What kind of parser the user asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomKind {
    /// A regular expression whose named groups become columns.
    Regex,
    /// A log4j / logback layout pattern.
    Log4j,
    /// JSON Lines, columns discovered from the first lines.
    JsonLines,
    /// logfmt, columns discovered from the first lines.
    Logfmt,
    /// Delimited with this delimiter; the first line is the header.
    Delimited(char),
}

/// Builds and checks a spec. `text` is the pattern (regex / log4j) and
/// ignored by the other kinds; `sample` are the first lines of the file.
pub fn build_spec(kind: CustomKind, text: &str, sample: &[String]) -> Result<ParserSpec, String> {
    let refs: Vec<&str> = sample.iter().map(String::as_str).collect();
    let spec = match kind {
        CustomKind::Regex => {
            if text.trim().is_empty() {
                return Err(
                    "Enter a regular expression with named groups, e.g. (?P<level>\\w+)".into(),
                );
            }
            ParserSpec::Regex {
                pattern: text.trim().to_string(),
                kinds: Default::default(),
            }
        }
        CustomKind::Log4j => {
            if text.trim().is_empty() {
                return Err("Enter a layout pattern, e.g. %d %-5p [%t] %c - %m%n".into());
            }
            // Report pattern errors with their own message.
            log4j_pattern_to_regex(text.trim()).map_err(|e| e.to_string())?;
            ParserSpec::Log4j {
                pattern: text.trim().to_string(),
                kinds: Default::default(),
            }
        }
        CustomKind::JsonLines => {
            let columns = discover_json_columns(&refs);
            if columns.is_empty() {
                return Err("No JSON objects among the first lines".into());
            }
            ParserSpec::JsonLines {
                columns,
                kinds: Default::default(),
            }
        }
        CustomKind::Logfmt => {
            let columns = discover_logfmt_columns(&refs);
            if columns.is_empty() {
                return Err("No key=value pairs among the first lines".into());
            }
            ParserSpec::Logfmt {
                columns,
                kinds: Default::default(),
            }
        }
        CustomKind::Delimited(d) => {
            let first = refs
                .iter()
                .find(|l| !l.trim().is_empty())
                .ok_or("The file has no lines yet")?;
            ParserSpec::delimited_from_header_line(first, d)
        }
    };
    spec.compile().map_err(|e| e.to_string())?;
    Ok(spec)
}

/// The state of the chooser window.
#[derive(Debug, Default)]
pub struct ChooserState {
    /// The window is open.
    pub open: bool,
    /// Text of the regex field.
    pub regex: String,
    /// Text of the log4j pattern field.
    pub log4j: String,
    /// Index into [`DELIMITERS`].
    pub delimiter: usize,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// The delimiters offered in the chooser.
pub const DELIMITERS: &[(char, &str)] = &[
    (',', "Comma"),
    ('\t', "Tab"),
    ('|', "Pipe"),
    (';', "Semicolon"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn regex_needs_named_groups_and_reports_errors() {
        let s = build_spec(CustomKind::Regex, r"^(?P<level>\w+) (?P<msg>.*)$", &[]).unwrap();
        assert!(matches!(s, ParserSpec::Regex { .. }));
        assert!(build_spec(CustomKind::Regex, "", &[]).is_err());
        let e = build_spec(CustomKind::Regex, "(unclosed", &[]).unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn log4j_patterns_are_checked() {
        let s = build_spec(CustomKind::Log4j, "%d %-5p [%t] %c - %m%n", &[]).unwrap();
        assert!(matches!(s, ParserSpec::Log4j { .. }));
        assert!(build_spec(CustomKind::Log4j, "  ", &[]).is_err());
    }

    #[test]
    fn json_and_logfmt_discover_their_columns() {
        let json = sample(&[
            r#"{"ts":"x","level":"info","msg":"a"}"#,
            r#"{"level":"warn","extra":1}"#,
        ]);
        match build_spec(CustomKind::JsonLines, "", &json).unwrap() {
            ParserSpec::JsonLines { columns, .. } => {
                assert!(columns.contains(&"level".to_string()));
                assert!(columns.contains(&"extra".to_string()));
            }
            other => panic!("{other:?}"),
        }
        assert!(build_spec(CustomKind::JsonLines, "", &sample(&["plain"])).is_err());
        let lf = sample(&["level=info msg=hello took=5ms"]);
        match build_spec(CustomKind::Logfmt, "", &lf).unwrap() {
            ParserSpec::Logfmt { columns, .. } => assert!(columns.contains(&"took".to_string())),
            other => panic!("{other:?}"),
        }
        assert!(build_spec(CustomKind::Logfmt, "", &[]).is_err());
    }

    #[test]
    fn delimited_takes_the_header_from_the_first_line() {
        let s = build_spec(
            CustomKind::Delimited(';'),
            "",
            &sample(&["", "id;name;city", "1;ann;Oslo"]),
        )
        .unwrap();
        let p = s.compile().unwrap();
        assert_eq!(p.schema().len(), 3);
        assert!(build_spec(CustomKind::Delimited(','), "", &[]).is_err());
    }
}
