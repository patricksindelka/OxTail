//! Built-in rule presets (PLAN.md section 7.2).
//!
//! Every preset is plain data ([`Rule`] values), so the settings UI can list,
//! toggle and copy them like user rules. All colours are semantic, so they
//! follow the active theme. Priorities: numbers -10, URLs and paths 5,
//! IPs and UUIDs 10, timestamps 20, HTTP 30, stack traces 40, log levels 50
//! (fatal 60); higher wins where matches overlap.

use crate::color::{ColorRef, SemanticColor as S};
use crate::rule::{Rule, RuleMatcher, Scope};
use crate::style::Style;

fn fg(c: S) -> Style {
    Style::fg(ColorRef::solid(c))
}

fn ci(name: &str, pattern: &str) -> Rule {
    Rule::new(
        name,
        RuleMatcher::Regex {
            pattern: pattern.to_owned(),
            case_sensitive: false,
        },
    )
}

/// One rule per log level: TRACE, DEBUG, INFO, WARN, ERROR, FATAL, including
/// their common abbreviations and single-letter forms (`[W]`, ` E `).
///
/// The single-letter form ` E ` needs whitespace on both sides, so an
/// isolated capital letter in prose can be highlighted too; disable the rule
/// or use the bracket form only if that bothers you.
pub fn log_levels() -> Vec<Rule> {
    let level = |name: &str, words: &str, letter: char, style: Style, prio: i32| {
        Rule::regex(name, format!(r"\b(?:{words})\b|\[{letter}\]|\s{letter}\s"))
            .styled(style)
            .with_priority(prio)
    };
    vec![
        level("level: trace", "TRACE|TRC|VERBOSE", 'T', fg(S::Trace), 50),
        level("level: debug", "DEBUG|DBG", 'D', fg(S::Debug), 50),
        level("level: info", "INFO|INF|NOTICE", 'I', fg(S::Info), 50),
        level(
            "level: warn",
            "WARN|WARNING|WRN",
            'W',
            fg(S::Warn).bold(),
            50,
        )
        .with_minimap(ColorRef::solid(S::Warn))
        .with_gutter(),
        level(
            "level: error",
            "ERROR|ERR|SEVERE",
            'E',
            fg(S::Error).bold(),
            50,
        )
        .with_minimap(ColorRef::solid(S::Error))
        .with_gutter(),
        level(
            "level: fatal",
            "FATAL|CRITICAL|CRIT|EMERG|EMERGENCY|PANIC|ALERT",
            'F',
            fg(S::Error).bold().on(ColorRef::subtle(S::Error)),
            60,
        )
        .with_minimap(ColorRef::solid(S::Error))
        .with_gutter(),
    ]
}

/// Timestamps: ISO 8601 / RFC 3339 (date, date-time, fractions, offsets),
/// clock times, syslog (`Sep 29 12:00:01`) and 13-digit epoch milliseconds.
pub fn timestamps() -> Vec<Rule> {
    let style = fg(S::Muted);
    vec![
        Rule::regex(
            "timestamp: ISO 8601",
            r"\b\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?(?:Z|[+-]\d{2}(?::?\d{2})?)?)?\b",
        ),
        Rule::regex("timestamp: time", r"\b\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?\b"),
        Rule::regex(
            "timestamp: syslog",
            r"\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}\b",
        ),
        Rule::regex("timestamp: epoch ms", r"\b1\d{12}\b"),
    ]
    .into_iter()
    .map(|r| r.styled(style).with_priority(20))
    .collect()
}

/// IPv4 (with optional port) and IPv6 addresses.
pub fn ip_addresses() -> Vec<Rule> {
    vec![
        Rule::regex(
            "IPv4",
            r"\b(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?::\d{1,5})?\b",
        )
        .styled(fg(S::Accent3))
        .with_priority(10),
        ci(
            "IPv6",
            r"\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b|\b(?:[0-9a-f]{1,4}:){1,6}(?::[0-9a-f]{1,4}){1,6}\b|\B::1\b",
        )
        .styled(fg(S::Accent3))
        .with_priority(10),
    ]
}

/// UUIDs (8-4-4-4-12 hex).
pub fn uuids() -> Vec<Rule> {
    vec![
        ci(
            "UUID",
            r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        )
        .styled(fg(S::Accent4))
        .with_priority(10),
    ]
}

/// `http(s)`, `ftp`, `ws(s)` and `file` URLs.
pub fn urls() -> Vec<Rule> {
    vec![
        ci("URL", r#"\b(?:https?|ftp|wss?|file)://[^\s<>"'`)\]]+"#)
            .styled(fg(S::Accent2).underline())
            .with_priority(5),
    ]
}

/// Unix paths (absolute, `~/`, `./`, with an optional `:line:col`) and
/// Windows paths (`C:\...`).
pub fn file_paths() -> Vec<Rule> {
    vec![
        Rule::regex(
            "path: unix",
            r#"(?:^|[\s"'=(\[])((?:~|\.{1,2})?/(?:[\w.@%+~-]+/)+[\w.@%+~-]*(?::\d+){0,2})"#,
        )
        .scoped(Scope::Group(1))
        .styled(fg(S::Accent6))
        .with_priority(5),
        Rule::regex(
            "path: windows",
            r#"\b[A-Za-z]:\\(?:[^\\/:*?"<>|\s]+\\)*[^\\/:*?"<>|\s]*"#,
        )
        .styled(fg(S::Accent6))
        .with_priority(5),
    ]
}

/// HTTP methods (when followed by a path or URL) and status codes by class:
/// 1xx muted, 2xx success, 3xx info, 4xx warn, 5xx error.
pub fn http() -> Vec<Rule> {
    let mut rules = vec![
        ci(
            "http: method",
            r"\b(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|CONNECT|TRACE)\s+(?:/|\*|https?://)",
        )
        .scoped(Scope::Group(1))
        .styled(fg(S::Accent1).bold())
        .with_priority(30),
    ];
    for (name, digit, style) in [
        ("1xx", '1', fg(S::Muted)),
        ("2xx", '2', fg(S::Success)),
        ("3xx", '3', fg(S::Info)),
        ("4xx", '4', fg(S::Warn).bold()),
        ("5xx", '5', fg(S::Error).bold()),
    ] {
        rules.push(
            ci(
                &format!("http: status {name}"),
                &format!(r#"(?:"\s+|\b(?:status|code|HTTP/\d(?:\.\d)?)[=:\s]+)({digit}\d\d)\b"#),
            )
            .scoped(Scope::Group(1))
            .styled(style)
            .with_priority(30),
        );
    }
    rules
}

/// Decimal and hexadecimal numbers (lowest priority).
pub fn numbers() -> Vec<Rule> {
    vec![
        Rule::regex("number", r"\b0x[0-9a-fA-F]+\b|\b\d+(?:\.\d+)?\b")
            .styled(fg(S::Accent7))
            .with_priority(-10),
    ]
}

/// Stack traces: Java/Kotlin frames and exception names, `Caused by:`,
/// Python `Traceback` and `File "...", line N`, Rust `panicked at`.
pub fn stack_traces() -> Vec<Rule> {
    let frame = fg(S::Muted);
    let error = fg(S::Error).bold();
    vec![
        Rule::regex("stack: java frame", r"^\s+at\s+[\w$.<>/-]+\([^)]*\)")
            .styled(frame)
            .with_priority(40),
        Rule::regex(
            "stack: exception",
            r"\b(?:[a-z_][\w]*\.)+[A-Z]\w*(?:Exception|Error|Throwable)\b",
        )
        .styled(error)
        .with_priority(45),
        Rule::regex("stack: caused by", r"^\s*Caused by:")
            .styled(error)
            .with_priority(45),
        Rule::regex(
            "stack: python traceback",
            r"^Traceback \(most recent call last\):",
        )
        .styled(error)
        .with_priority(45)
        .with_minimap(ColorRef::solid(S::Error)),
        Rule::regex(
            "stack: python frame",
            r#"^\s+File "[^"]+", line \d+(?:, in \S+)?"#,
        )
        .styled(frame)
        .with_priority(40),
        Rule::regex("stack: rust panic", r"\bpanicked at\b")
            .styled(error)
            .with_priority(45)
            .with_minimap(ColorRef::solid(S::Error))
            .with_gutter(),
    ]
}

/// A sensible default set combining all presets.
pub fn default_rules() -> Vec<Rule> {
    let mut rules = Vec::new();
    rules.extend(numbers());
    rules.extend(urls());
    rules.extend(file_paths());
    rules.extend(ip_addresses());
    rules.extend(uuids());
    rules.extend(timestamps());
    rules.extend(http());
    rules.extend(stack_traces());
    rules.extend(log_levels());
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompiledRules;

    #[test]
    fn every_preset_compiles() {
        for rules in [
            log_levels(),
            timestamps(),
            ip_addresses(),
            uuids(),
            urls(),
            file_paths(),
            http(),
            numbers(),
            stack_traces(),
            default_rules(),
        ] {
            CompiledRules::compile(&rules).unwrap();
        }
    }

    #[test]
    fn presets_serialise_to_toml() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct File {
            rules: Vec<Rule>,
        }
        let f = File {
            rules: default_rules(),
        };
        let text = toml::to_string(&f).unwrap();
        assert_eq!(toml::from_str::<File>(&text).unwrap(), f);
    }
}
